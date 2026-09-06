//! Headless Web Edition server.
//!
//! This module deliberately has no Tauri runtime dependency. All filesystem
//! operations start from a server-side project registry and accept only
//! project-relative paths.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{
    DefaultBodyLimit, FromRequest, Multipart, Path as AxumPath, Query, Request, State,
};
use axum::http::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, COOKIE, ETAG,
    HOST, IF_MATCH, ORIGIN, RANGE, SET_COOKIE,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures::stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::watch;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use walkdir::WalkDir;

const API_PREFIX: &str = "/api/v2";
const SESSION_COOKIE: &str = "llm_wiki_session";
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
const SESSION_TTL_SECS: u64 = 12 * 60 * 60;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub workspace_root: PathBuf,
    pub data_dir: PathBuf,
    pub web_root: PathBuf,
    pub bootstrap_token: String,
    pub allow_insecure_remote: bool,
    pub secure_cookie: bool,
    pub app_state: Option<PathBuf>,
}

impl ServerConfig {
    pub fn from_args<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut values = args.into_iter().map(Into::into);
        let _program = values.next();
        let mut options = HashMap::<String, String>::new();
        let mut flags = Vec::new();
        while let Some(arg) = values.next() {
            if !arg.starts_with("--") {
                return Err(format!("unexpected argument: {arg}"));
            }
            if matches!(arg.as_str(), "--allow-insecure-remote" | "--secure-cookie") {
                flags.push(arg);
                continue;
            }
            if !matches!(
                arg.as_str(),
                "--host"
                    | "--port"
                    | "--workspace-root"
                    | "--data-dir"
                    | "--web-root"
                    | "--token"
                    | "--app-state"
            ) {
                return Err(format!("unknown option: {arg}"));
            }
            let value = values
                .next()
                .ok_or_else(|| format!("missing value for {arg}"))?;
            options.insert(arg, value);
        }
        let env_or = |arg: &str, env: &str| {
            options
                .get(arg)
                .cloned()
                .or_else(|| std::env::var(env).ok())
        };
        let host = env_or("--host", "LLM_WIKI_HOST")
            .unwrap_or_else(|| "127.0.0.1".to_string())
            .parse::<IpAddr>()
            .map_err(|e| format!("invalid host: {e}"))?;
        let port = env_or("--port", "LLM_WIKI_PORT")
            .unwrap_or_else(|| "19828".to_string())
            .parse::<u16>()
            .map_err(|e| format!("invalid port: {e}"))?;
        let workspace_root = env_or("--workspace-root", "LLM_WIKI_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .ok_or("--workspace-root or LLM_WIKI_WORKSPACE_ROOT is required")?;
        let data_dir = env_or("--data-dir", "LLM_WIKI_DATA_DIR")
            .map(PathBuf::from)
            .ok_or("--data-dir or LLM_WIKI_DATA_DIR is required")?;
        let web_root = env_or("--web-root", "LLM_WIKI_WEB_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("dist"));
        let bootstrap_token = env_or("--token", "LLM_WIKI_BOOTSTRAP_TOKEN")
            .filter(|value| !value.trim().is_empty())
            .ok_or("--token or LLM_WIKI_BOOTSTRAP_TOKEN is required")?;
        if bootstrap_token.chars().count() < 32 {
            return Err(
                "--token or LLM_WIKI_BOOTSTRAP_TOKEN must be at least 32 characters".to_string(),
            );
        }
        let allow_insecure_remote = flags.iter().any(|v| v == "--allow-insecure-remote")
            || env_truthy("LLM_WIKI_ALLOW_INSECURE_REMOTE");
        let secure_cookie =
            flags.iter().any(|v| v == "--secure-cookie") || env_truthy("LLM_WIKI_SECURE_COOKIE");
        if !host.is_loopback() && !allow_insecure_remote {
            return Err("refusing non-loopback plaintext bind; use a TLS reverse proxy on loopback or explicitly pass --allow-insecure-remote".to_string());
        }
        let app_state = env_or("--app-state", "LLM_WIKI_APP_STATE").map(PathBuf::from);
        Ok(Self {
            host,
            port,
            workspace_root,
            data_dir,
            web_root,
            bootstrap_token,
            allow_insecure_remote,
            secure_cookie,
            app_state,
        })
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    #[serde(skip_serializing)]
    path: PathBuf,
    created_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryProject<'a> {
    id: &'a str,
    name: &'a str,
    path: String,
    created_at: u64,
}

#[derive(Debug, Clone)]
struct Session {
    csrf: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    project_id: Option<String>,
    #[serde(rename = "type")]
    job_type: String,
    status: String,
    #[serde(default)]
    progress: JobProgress,
    created_at: u64,
    updated_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSession {
    id: String,
    project_id: String,
    title: Option<String>,
    created_at: u64,
    updated_at: u64,
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    id: String,
    role: String,
    content: String,
    created_at: u64,
}

#[derive(Clone)]
struct ChatProviderConfig {
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

#[derive(Clone, Serialize)]
struct ChatStreamEvent {
    event: String,
    data: Value,
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<ServerConfig>,
    projects: Arc<Mutex<BTreeMap<String, Project>>>,
    project_write_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    jobs: Arc<Mutex<BTreeMap<String, Job>>>,
    events: tokio::sync::broadcast::Sender<Job>,
    settings: Arc<Mutex<Value>>,
    chat_sessions: Arc<Mutex<BTreeMap<String, ChatSession>>>,
    chat_turns: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    client: reqwest::Client,
}

impl AppState {
    pub fn new(config: ServerConfig) -> Result<Self, String> {
        fs::create_dir_all(&config.workspace_root)
            .map_err(|e| format!("create workspace root: {e}"))?;
        fs::create_dir_all(&config.data_dir).map_err(|e| format!("create data dir: {e}"))?;
        let workspace_root = fs::canonicalize(&config.workspace_root)
            .map_err(|e| format!("canonicalize workspace root: {e}"))?;
        let data_dir = fs::canonicalize(&config.data_dir)
            .map_err(|e| format!("canonicalize data dir: {e}"))?;
        let web_root = canonicalize_web_root(&config.web_root)?;
        let mut config = config;
        config.workspace_root = workspace_root;
        config.data_dir = data_dir;
        config.web_root = web_root;
        let projects = load_registry(&config)?;
        let settings = load_settings(&config)?;
        let jobs = load_jobs(&config)?;
        let chat_sessions = load_chat_sessions(&config)?;
        let (events, _) = tokio::sync::broadcast::channel(128);
        Ok(Self {
            config: Arc::new(config),
            projects: Arc::new(Mutex::new(projects)),
            project_write_locks: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            jobs: Arc::new(Mutex::new(jobs)),
            events,
            settings: Arc::new(Mutex::new(settings)),
            chat_sessions: Arc::new(Mutex::new(chat_sessions)),
            chat_turns: Arc::new(Mutex::new(HashMap::new())),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())?,
        })
    }
}

pub fn run_from_env() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", usage());
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("llm-wiki-server {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let config = ServerConfig::from_args(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(run(config))
}

fn usage() -> &'static str {
    "llm-wiki-server\n\nUSAGE:\n  llm-wiki-server --workspace-root PATH --data-dir PATH --token TOKEN [OPTIONS]\n\nOPTIONS:\n  --host IP                    Bind IP (default: 127.0.0.1)\n  --port PORT                  Bind port (default: 19828)\n  --workspace-root PATH        Primary project workspace\n  --data-dir PATH              Server control-plane data\n  --web-root PATH              Built SPA directory (default: dist)\n  --token TOKEN                Bootstrap login and Bearer token\n  --app-state PATH             Optional desktop app-state.json import\n  --secure-cookie              Add Secure to the session cookie\n  --allow-insecure-remote      Explicitly allow plaintext non-loopback bind\n  -h, --help                   Print help\n  -V, --version                Print version\n\nEquivalent environment variables use the LLM_WIKI_* prefix."
}

pub async fn run(config: ServerConfig) -> Result<(), String> {
    let addr = SocketAddr::new(config.host, config.port);
    let state = AppState::new(config)?;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    eprintln!("llm-wiki-server listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/auth/logout", post(logout))
        .route("/auth/session", get(auth_session))
        .route("/auth/csrf", get(refresh_csrf).post(refresh_csrf))
        .route("/system/capabilities", get(capabilities))
        .route("/settings", get(get_settings).patch(patch_settings))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/register", post(register_project))
        .route(
            "/projects/{project_id}",
            get(get_project_handler)
                .patch(update_project)
                .delete(delete_project),
        )
        .route("/projects/{project_id}/tree", get(project_tree))
        .route(
            "/projects/{project_id}/files",
            get(file_tree).delete(delete_file),
        )
        .route(
            "/projects/{project_id}/files/content",
            get(read_text).put(write_text),
        )
        .route("/projects/{project_id}/files/move", post(move_file))
        .route("/projects/{project_id}/directories", post(create_directory))
        .route("/projects/{project_id}/uploads", post(upload_file))
        .route("/projects/{project_id}/assets", get(asset_query))
        .route("/projects/{project_id}/assets/{*path}", get(asset_wildcard))
        .route("/projects/{project_id}/search", post(search_project))
        .route("/projects/{project_id}/graph", get(graph_project))
        .route("/projects/{project_id}/reviews", get(list_reviews))
        .route(
            "/projects/{project_id}/reviews/{review_id}",
            patch(update_review),
        )
        .route("/projects/{project_id}/chat", post(chat))
        .route(
            "/projects/{project_id}/chat/sessions",
            get(list_chat_sessions).post(create_chat_session),
        )
        .route(
            "/projects/{project_id}/chat/sessions/{session_id}",
            get(get_chat_session),
        )
        .route(
            "/projects/{project_id}/chat/sessions/{session_id}/turns",
            post(chat_turn),
        )
        .route(
            "/projects/{project_id}/chat/sessions/{session_id}/cancel",
            post(cancel_chat_turn),
        )
        .route("/projects/{project_id}/index/rebuild", post(rebuild_index))
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{job_id}", get(get_job))
        .route("/jobs/{job_id}/events", get(job_events))
        .route("/jobs/{job_id}/cancel", post(cancel_job))
        .route("/jobs/{job_id}/retry", post(retry_job))
        .route("/events", get(project_events))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/api/v2/health/live", get(health_live))
        .route("/api/v2/health/ready", get(health_ready))
        .route("/api/v2/auth/login", post(login))
        .nest(API_PREFIX, protected)
        .fallback(static_file)
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .with_state(state)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: json!({}),
        }
    }
    fn conflict(expected: Option<String>, actual: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "FILE_REVISION_CONFLICT",
            message: "File was modified by another client".to_string(),
            details: json!({"expected": expected, "actual": actual}),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = format!("req_{}", Uuid::new_v4());
        (
            self.status,
            Json(json!({"error": {"code": self.code, "message": self.message, "requestId": request_id, "details": self.details}})),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn health_live() -> Json<Value> {
    Json(json!({"status": "live", "version": env!("CARGO_PKG_VERSION")}))
}

async fn health_ready(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    if !state.config.workspace_root.is_dir() || !state.config.data_dir.is_dir() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_READY",
            "workspace or data directory is unavailable",
        ));
    }
    Ok(Json(json!({"status": "ready"})))
}

#[derive(Deserialize)]
struct LoginRequest {
    token: String,
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<LoginRequest>,
) -> ApiResult<Response> {
    validate_optional_origin(&headers)?;
    if !constant_time_eq(
        input.token.as_bytes(),
        state.config.bootstrap_token.as_bytes(),
    ) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "INVALID_CREDENTIALS",
            "Invalid login token",
        ));
    }
    let session_id = Uuid::new_v4().to_string();
    let csrf = Uuid::new_v4().to_string();
    state.sessions.lock().unwrap().insert(
        hash_string(&session_id),
        Session {
            csrf: csrf.clone(),
            expires_at: now_secs() + SESSION_TTL_SECS,
        },
    );
    let mut cookie = format!(
        "{SESSION_COOKIE}={session_id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_TTL_SECS}"
    );
    if state.config.secure_cookie {
        cookie.push_str("; Secure");
    }
    let mut response = Json(json!({"authenticated": true, "csrfToken": csrf})).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "COOKIE_ERROR",
                e.to_string(),
            )
        })?,
    );
    Ok(response)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(session_id) = cookie_value(&headers, SESSION_COOKIE) {
        state
            .sessions
            .lock()
            .unwrap()
            .remove(&hash_string(&session_id));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static("llm_wiki_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    Ok(response)
}

async fn auth_session(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let auth = authenticate(&state, &headers)?;
    Ok(Json(json!({
        "authenticated": true,
        "kind": auth.kind,
        "csrfToken": auth.csrf,
        "capabilities": ["projects", "files", "search", "graph", "jobs"]
    })))
}

async fn refresh_csrf(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let session_id = cookie_value(&headers, SESSION_COOKIE).ok_or_else(unauthorized)?;
    let key = hash_string(&session_id);
    let mut sessions = state.sessions.lock().unwrap();
    let session = sessions.get_mut(&key).ok_or_else(unauthorized)?;
    session.csrf = Uuid::new_v4().to_string();
    session.expires_at = now_secs() + SESSION_TTL_SECS;
    Ok(Json(json!({"csrfToken": session.csrf})))
}

async fn capabilities(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "runtime": "web",
        "desktop": false,
        "fileApi": true,
        "search": true,
        "graph": true,
        "chat": chat_provider_config(&state).is_ok(),
        "jobs": true
    }))
}

async fn get_settings(State(state): State<AppState>) -> Json<Value> {
    Json(redact_settings(&state.settings.lock().unwrap()))
}

async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    if !patch.is_object() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_SETTINGS",
            "settings patch must be a JSON object",
        ));
    }
    let updated = {
        let mut settings = state.settings.lock().unwrap();
        let mut candidate = settings.clone();
        merge_json(&mut candidate, patch);
        persist_settings(&state.config, &candidate)?;
        *settings = candidate.clone();
        candidate
    };
    Ok(Json(redact_settings(&updated)))
}

#[derive(Clone)]
struct AuthInfo {
    kind: &'static str,
    csrf: Option<String>,
}

async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> ApiResult<Response> {
    let auth = authenticate(&state, request.headers())?;
    if is_write_method(request.method()) {
        validate_optional_origin(request.headers())?;
        // A session needs this endpoint to obtain its first CSRF token. It is
        // still authenticated and Origin-checked when an Origin is supplied.
        let is_csrf_refresh = request.uri().path().ends_with("/auth/csrf");
        if auth.kind == "session" && !is_csrf_refresh {
            validate_required_origin(request.headers())?;
            let supplied = request
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let expected = auth.csrf.as_deref().unwrap_or("");
            if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "CSRF_REJECTED",
                    "Missing or invalid CSRF token",
                ));
            }
        }
    }
    Ok(next.run(request).await)
}

fn authenticate(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthInfo> {
    if let Some(value) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        if constant_time_eq(value.as_bytes(), state.config.bootstrap_token.as_bytes()) {
            return Ok(AuthInfo {
                kind: "bearer",
                csrf: None,
            });
        }
    }
    if let Some(id) = cookie_value(headers, SESSION_COOKIE) {
        let key = hash_string(&id);
        let mut sessions = state.sessions.lock().unwrap();
        sessions.retain(|_, value| value.expires_at > now_secs());
        if let Some(session) = sessions.get(&key) {
            return Ok(AuthInfo {
                kind: "session",
                csrf: Some(session.csrf.clone()),
            });
        }
    }
    Err(unauthorized())
}

fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "Authentication required",
    )
}

fn is_write_method(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn validate_optional_origin(headers: &HeaderMap) -> ApiResult<()> {
    if headers.contains_key(ORIGIN) {
        validate_required_origin(headers)
    } else {
        Ok(())
    }
}

fn validate_required_origin(headers: &HeaderMap) -> ApiResult<()> {
    let origin = headers
        .get(ORIGIN)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "ORIGIN_REJECTED",
                "Origin is required",
            )
        })?;
    let host = headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(StatusCode::FORBIDDEN, "ORIGIN_REJECTED", "Host is required")
        })?;
    let valid = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|authority| authority == host);
    if !valid {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "Origin does not match Host",
        ));
    }
    Ok(())
}

async fn list_projects(State(state): State<AppState>) -> Json<Value> {
    let projects: Vec<_> = state.projects.lock().unwrap().values().cloned().collect();
    Json(json!({"projects": projects}))
}

async fn get_project_handler(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<Json<Project>> {
    Ok(Json(get_project(&state, &project_id)?))
}

#[derive(Deserialize)]
struct UpdateProjectRequest {
    name: String,
}

async fn update_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<UpdateProjectRequest>,
) -> ApiResult<Json<Project>> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_NAME",
            "name is required",
        ));
    }
    let updated = {
        let mut projects = state.projects.lock().unwrap();
        let mut candidate = projects.clone();
        let project = candidate.get_mut(&project_id).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "PROJECT_NOT_FOUND",
                "project not found",
            )
        })?;
        project.name = name.to_string();
        let updated = project.clone();
        persist_registry(&state.config, &candidate)?;
        *projects = candidate;
        updated
    };
    Ok(Json(updated))
}

async fn delete_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<StatusCode> {
    let mut projects = state.projects.lock().unwrap();
    let mut candidate = projects.clone();
    if candidate.remove(&project_id).is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "PROJECT_NOT_FOUND",
            "project not found",
        ));
    }
    persist_registry(&state.config, &candidate)?;
    *projects = candidate;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectRequest {
    name: String,
    directory_name: Option<String>,
}

async fn create_project(
    State(state): State<AppState>,
    Json(input): Json<CreateProjectRequest>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    let directory = input.directory_name.unwrap_or_else(|| slugify(&input.name));
    validate_project_directory(&directory)?;
    if input.name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_NAME",
            "name is required",
        ));
    }
    let root = state.config.workspace_root.join(&directory);
    match fs::create_dir(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "PROJECT_EXISTS",
                "project directory already exists",
            ));
        }
        Err(error) => return Err(io_error(error)),
    }
    if let Err(error) = create_project_layout(&root) {
        return Err(rollback_new_project_directory(&root, error));
    }
    let project = Project {
        id: Uuid::new_v4().to_string(),
        name: input.name.trim().to_string(),
        path: match fs::canonicalize(&root).map_err(io_error) {
            Ok(path) => path,
            Err(error) => return Err(rollback_new_project_directory(&root, error)),
        },
        created_at: now_secs(),
    };
    {
        let mut projects = state.projects.lock().unwrap();
        let mut candidate = projects.clone();
        candidate.insert(project.id.clone(), project.clone());
        if let Err(error) = persist_registry(&state.config, &candidate) {
            return Err(rollback_new_project_directory(&root, error));
        }
        *projects = candidate;
    }
    Ok((StatusCode::CREATED, Json(project)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterProjectRequest {
    relative_path: String,
    name: Option<String>,
}

async fn register_project(
    State(state): State<AppState>,
    Json(input): Json<RegisterProjectRequest>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    let path = workspace_relative(&state.config.workspace_root, &input.relative_path)?;
    if !path.is_dir() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "PROJECT_NOT_FOUND",
            "project directory does not exist",
        ));
    }
    let canonical = fs::canonicalize(&path).map_err(io_error)?;
    let mut projects = state.projects.lock().unwrap();
    if projects.values().any(|p| p.path == canonical) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "PROJECT_EXISTS",
            "project is already registered",
        ));
    }
    let name = input
        .name
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            canonical
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("Project")
                .to_string()
        });
    let project = Project {
        id: Uuid::new_v4().to_string(),
        name,
        path: canonical,
        created_at: now_secs(),
    };
    let mut candidate = projects.clone();
    candidate.insert(project.id.clone(), project.clone());
    persist_registry(&state.config, &candidate)?;
    *projects = candidate;
    Ok((StatusCode::CREATED, Json(project)))
}

#[derive(Deserialize)]
struct FileQuery {
    path: Option<String>,
    revision: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    path: String,
    name: String,
    kind: &'static str,
    size: u64,
    modified_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileTreeNode {
    name: String,
    path: String,
    is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    children: Option<Vec<FileTreeNode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

async fn project_tree(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    Ok(Json(
        json!({"tree": collect_file_tree(&project.path, &project.path)?}),
    ))
}

async fn file_tree(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    let relative = query.path.unwrap_or_default();
    let directory = safe_project_path(&project.path, &relative, true)?;
    if !directory.is_dir() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_A_DIRECTORY",
            "directory not found",
        ));
    }
    let mut entries = Vec::new();
    for item in fs::read_dir(directory).map_err(io_error)? {
        let item = item.map_err(io_error)?;
        let name = item.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let metadata = fs::symlink_metadata(item.path()).map_err(io_error)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        entries.push(FileEntry {
            path,
            name,
            kind: if metadata.is_dir() {
                "directory"
            } else {
                "file"
            },
            size: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            modified_at: modified_secs(&metadata),
        });
    }
    entries.sort_by(|a, b| a.kind.cmp(b.kind).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(json!({"path": relative, "entries": entries})))
}

async fn read_text(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Response> {
    let relative = required_path(query.path)?;
    let project = get_project(&state, &project_id)?;
    let path = safe_project_path(&project.path, &relative, false)?;
    let metadata = fs::metadata(&path).map_err(not_found_io)?;
    if metadata.len() > MAX_TEXT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "FILE_TOO_LARGE",
            "text file is too large",
        ));
    }
    let bytes = fs::read(&path).map_err(not_found_io)?;
    let content = String::from_utf8(bytes.clone()).map_err(|_| {
        ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "NOT_TEXT",
            "file is not valid UTF-8",
        )
    })?;
    let revision = revision(&bytes);
    let mut response =
        Json(json!({"path": relative, "content": content, "revision": revision})).into_response();
    response.headers_mut().insert(ETAG, quoted_etag(&revision)?);
    Ok(response)
}

#[derive(Deserialize)]
struct WriteTextRequest {
    content: String,
    revision: Option<String>,
}

async fn write_text(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
    Json(input): Json<WriteTextRequest>,
) -> ApiResult<Json<Value>> {
    let relative = required_path(query.path)?;
    if input.content.len() as u64 > MAX_TEXT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "FILE_TOO_LARGE",
            "text file is too large",
        ));
    }
    let project = get_project(&state, &project_id)?;
    let project_lock = project_write_lock(&state, &project_id);
    let _project_guard = project_lock.lock().unwrap();
    let path = safe_project_path(&project.path, &relative, false)?;
    let supplied = supplied_revision(&headers, input.revision)?;
    if path.exists() {
        let bytes = fs::read(&path).map_err(io_error)?;
        let actual = revision(&bytes);
        if supplied.as_deref() != Some(actual.as_str()) {
            return Err(ApiError::conflict(supplied, actual));
        }
    } else if supplied.as_deref().is_some_and(|value| value != "*") {
        return Err(ApiError::conflict(supplied, "missing".to_string()));
    }
    atomic_write(&path, input.content.as_bytes())?;
    let new_revision = revision(input.content.as_bytes());
    Ok(Json(json!({
        "path": relative,
        "content": input.content,
        "revision": new_revision,
        "mimeType": mime_guess::from_path(&path).first_or_text_plain().essence_str(),
        "updatedAt": modified_secs(&fs::metadata(&path).map_err(io_error)?)
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveFileRequest {
    source_path: String,
    target_path: String,
}

async fn move_file(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<MoveFileRequest>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    let project_lock = project_write_lock(&state, &project_id);
    let _project_guard = project_lock.lock().unwrap();
    let source = safe_project_path(&project.path, &input.source_path, false)?;
    let target = safe_project_path(&project.path, &input.target_path, false)?;
    let source_metadata = fs::symlink_metadata(&source).map_err(not_found_io)?;
    if !source_metadata.is_file() && !source_metadata.is_dir() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_FILE_TYPE",
            "unsupported file type",
        ));
    }
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "FILE_EXISTS",
                "move target already exists",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    if source_metadata.is_dir() && target.starts_with(&source) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_PATH",
            "cannot move a directory into itself",
        ));
    }
    fs::rename(&source, &target).map_err(io_error)?;
    Ok(Json(json!({
        "sourcePath": input.source_path,
        "targetPath": input.target_path,
    })))
}

#[derive(Deserialize)]
struct PathRequest {
    path: String,
}

#[derive(Deserialize)]
struct DeleteFileRequest {
    revision: Option<String>,
}

async fn create_directory(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<PathRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let project = get_project(&state, &project_id)?;
    let project_lock = project_write_lock(&state, &project_id);
    let _project_guard = project_lock.lock().unwrap();
    let path = safe_project_path(&project.path, &input.path, false)?;
    fs::create_dir_all(path).map_err(io_error)?;
    Ok((StatusCode::CREATED, Json(json!({"path": input.path}))))
}

async fn delete_file(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
    body: Option<Json<DeleteFileRequest>>,
) -> ApiResult<StatusCode> {
    let relative = required_path(query.path)?;
    let project = get_project(&state, &project_id)?;
    let project_lock = project_write_lock(&state, &project_id);
    let _project_guard = project_lock.lock().unwrap();
    let path = safe_project_path(&project.path, &relative, false)?;
    let metadata = fs::symlink_metadata(&path).map_err(not_found_io)?;
    if metadata.is_file() {
        let actual = revision(&fs::read(&path).map_err(io_error)?);
        let requested_revision =
            merge_revision(query.revision, body.and_then(|Json(body)| body.revision))?;
        let supplied = supplied_revision(&headers, requested_revision)?;
        if supplied.is_some() && supplied.as_deref() != Some(actual.as_str()) {
            return Err(ApiError::conflict(supplied, actual));
        }
        fs::remove_file(path).map_err(io_error)?;
    } else if metadata.is_dir() {
        fs::remove_dir(path).map_err(|e| {
            ApiError::new(StatusCode::CONFLICT, "DIRECTORY_NOT_EMPTY", e.to_string())
        })?;
    } else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_FILE_TYPE",
            "unsupported file type",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_file(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<FileQuery>,
    request: Request,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let project = get_project(&state, &project_id)?;
    let directory = query.path.unwrap_or_default();
    let headers = request.headers().clone();
    let multipart = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"));
    let mut uploads = Vec::new();
    if multipart {
        let mut form = Multipart::from_request(request, &state)
            .await
            .map_err(|error| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "INVALID_MULTIPART",
                    error.to_string(),
                )
            })?;
        while let Some(field) = form.next_field().await.map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "INVALID_MULTIPART",
                error.to_string(),
            )
        })? {
            if field.name() != Some("files") {
                continue;
            }
            let name = field.file_name().map(ToOwned::to_owned).ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "FILE_NAME_REQUIRED",
                    "each multipart file needs a filename",
                )
            })?;
            let bytes = field.bytes().await.map_err(|error| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "INVALID_MULTIPART",
                    error.to_string(),
                )
            })?;
            uploads.push((name, bytes));
        }
    } else {
        let name = headers
            .get("x-file-name")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "FILE_NAME_REQUIRED",
                    "X-File-Name header is required for non-multipart uploads",
                )
            })?
            .to_string();
        uploads.push((
            name,
            to_bytes(request.into_body(), MAX_UPLOAD_BYTES)
                .await
                .map_err(|error| {
                    ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "UPLOAD_TOO_LARGE",
                        error.to_string(),
                    )
                })?,
        ));
    }
    if uploads.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "FILES_REQUIRED",
            "multipart uploads must contain one or more files fields",
        ));
    }
    let project_lock = project_write_lock(&state, &project_id);
    let _project_guard = project_lock.lock().unwrap();
    let mut targets = Vec::with_capacity(uploads.len());
    for (file_name, bytes) in uploads {
        validate_file_name(&file_name)?;
        let relative = if directory.trim().is_empty() {
            file_name
        } else {
            format!("{}/{}", directory.trim_matches('/'), file_name)
        };
        let target = safe_project_path(&project.path, &relative, false)?;
        if target.exists()
            || targets
                .iter()
                .any(|(path, _, _): &(PathBuf, String, Bytes)| path == &target)
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "FILE_EXISTS",
                "upload target already exists or was supplied more than once",
            ));
        }
        targets.push((target, relative, bytes));
    }
    let mut items = Vec::with_capacity(targets.len());
    for (target, relative, bytes) in targets {
        atomic_write(&target, &bytes)?;
        items.push(json!({"path": relative, "revision": revision(&bytes), "size": bytes.len()}));
    }
    Ok((StatusCode::CREATED, Json(json!({"items": items}))))
}

#[derive(Deserialize)]
struct AssetQuery {
    path: Option<String>,
    download: Option<bool>,
}

async fn asset_query(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<AssetQuery>,
    headers: HeaderMap,
    method: Method,
) -> ApiResult<Response> {
    serve_asset(
        &state,
        &project_id,
        &required_path(query.path)?,
        query.download.unwrap_or(false),
        headers,
        method,
    )
    .await
}

async fn asset_wildcard(
    State(state): State<AppState>,
    AxumPath((project_id, relative)): AxumPath<(String, String)>,
    Query(query): Query<AssetQuery>,
    headers: HeaderMap,
    method: Method,
) -> ApiResult<Response> {
    serve_asset(
        &state,
        &project_id,
        &relative,
        query.download.unwrap_or(false),
        headers,
        method,
    )
    .await
}

async fn serve_asset(
    state: &AppState,
    project_id: &str,
    relative: &str,
    download: bool,
    headers: HeaderMap,
    method: Method,
) -> ApiResult<Response> {
    let project = get_project(state, project_id)?;
    let path = safe_project_path(&project.path, relative, false)?;
    let size = fs::metadata(&path).map_err(not_found_io)?.len();
    let (status, start, end) = parse_range(headers.get(RANGE).and_then(|v| v.to_str().ok()), size)?;
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let active_content = is_active_content(&mime, &path);
    let disposition = if download || active_content {
        "attachment"
    } else if mime.type_() == mime::TEXT
        || mime.type_() == mime::IMAGE
        || mime.type_() == mime::AUDIO
        || mime.type_() == mime::VIDEO
        || mime == mime::APPLICATION_PDF
    {
        "inline"
    } else {
        "attachment"
    };
    let filename = path.file_name().and_then(|v| v.to_str()).unwrap_or("asset");
    let body = stream_file_body(&path, start, length, method == Method::HEAD).await?;
    let mut response = (status, body).into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap());
    response_headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    response_headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response_headers.insert(
        CONTENT_DISPOSITION,
        content_disposition_header(disposition, filename)?,
    );
    if active_content {
        response_headers.insert(
            "content-security-policy",
            HeaderValue::from_static("sandbox"),
        );
    }
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap(),
        );
    }
    Ok(response)
}

fn content_disposition_header(disposition: &str, filename: &str) -> ApiResult<HeaderValue> {
    let fallback = filename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let fallback = if fallback.trim_matches('_').is_empty() {
        "asset".to_string()
    } else {
        fallback
    };
    let encoded = filename
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    HeaderValue::from_str(&format!(
        "{disposition}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .map_err(|error| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "HEADER_ERROR",
            error.to_string(),
        )
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest {
    query: String,
    top_k: Option<usize>,
    include_content: Option<bool>,
}

async fn search_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<SearchRequest>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    let terms: Vec<String> = input
        .query
        .split_whitespace()
        .map(|v| v.to_lowercase())
        .collect();
    if terms.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "QUERY_REQUIRED",
            "query is required",
        ));
    }
    let mut results = Vec::<Value>::new();
    for item in WalkDir::new(&project.path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !item.file_type().is_file()
            || item.path().extension().and_then(|v| v.to_str()) != Some("md")
            || is_hidden_relative(&project.path, item.path())
        {
            continue;
        }
        let Ok(content) = fs::read_to_string(item.path()) else {
            continue;
        };
        let lower = content.to_lowercase();
        let score: usize = terms.iter().map(|term| lower.matches(term).count()).sum();
        if score == 0 {
            continue;
        }
        let relative = relative_string(&project.path, item.path())?;
        let title = markdown_title(&content).unwrap_or_else(|| {
            item.path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
        let snippet = search_snippet(&content, &terms);
        results.push(json!({"path": relative, "title": title, "score": score, "snippet": snippet, "content": input.include_content.unwrap_or(false).then_some(content)}));
    }
    results.sort_by(|a, b| {
        b.get("score")
            .and_then(Value::as_u64)
            .cmp(&a.get("score").and_then(Value::as_u64))
    });
    results.truncate(input.top_k.unwrap_or(20).clamp(1, 100));
    Ok(Json(json!({"mode": "keyword", "results": results})))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GraphNode {
    id: String,
    label: String,
    node_type: String,
    path: String,
    link_count: usize,
}
#[derive(Serialize)]
struct GraphEdge {
    source: String,
    target: String,
    weight: f32,
}

async fn graph_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    let mut pages = BTreeMap::<String, (String, String, Vec<String>)>::new();
    for item in WalkDir::new(&project.path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !item.file_type().is_file()
            || item.path().extension().and_then(|v| v.to_str()) != Some("md")
            || is_hidden_relative(&project.path, item.path())
        {
            continue;
        }
        let Ok(content) = fs::read_to_string(item.path()) else {
            continue;
        };
        let relative = relative_string(&project.path, item.path())?;
        let id = relative.trim_end_matches(".md").to_string();
        let label = markdown_title(&content)
            .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(&id).to_string());
        pages.insert(id, (label, relative, wiki_links(&content)));
    }
    let ids: Vec<String> = pages.keys().cloned().collect();
    let mut counts = HashMap::<String, usize>::new();
    let mut edges = Vec::new();
    for (source, (_, _, links)) in &pages {
        for link in links {
            if let Some(target) = resolve_graph_link(link, &ids) {
                *counts.entry(source.clone()).or_default() += 1;
                *counts.entry(target.clone()).or_default() += 1;
                edges.push(GraphEdge {
                    source: source.clone(),
                    target,
                    weight: 1.0,
                });
            }
        }
    }
    let nodes: Vec<_> = pages
        .into_iter()
        .map(|(id, (label, path, _))| GraphNode {
            link_count: *counts.get(&id).unwrap_or(&0),
            id,
            label,
            node_type: graph_type(&path),
            path,
        })
        .collect();
    Ok(Json(json!({"nodes": nodes, "edges": edges})))
}

async fn list_reviews(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    let reviews = read_reviews(&project.path)?;
    Ok(Json(json!({"items": reviews})))
}

async fn update_review(
    State(state): State<AppState>,
    AxumPath((project_id, review_id)): AxumPath<(String, String)>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    let project = get_project(&state, &project_id)?;
    let project_lock = project_write_lock(&state, &project_id);
    let _project_guard = project_lock.lock().unwrap();
    let mut reviews = read_reviews(&project.path)?;
    let item = reviews
        .iter_mut()
        .find(|v| v.get("id").and_then(Value::as_str) == Some(&review_id))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "REVIEW_NOT_FOUND",
                "review not found",
            )
        })?;
    if let (Some(target), Some(source)) = (item.as_object_mut(), patch.as_object()) {
        for key in ["status", "action", "resolved", "resolution", "notes"] {
            if let Some(value) = source.get(key) {
                target.insert(key.to_string(), value.clone());
            }
        }
        if let Some(status) = source.get("status").and_then(Value::as_str) {
            target.insert("resolved".to_string(), Value::Bool(status == "resolved"));
        }
    }
    let result = item.clone();
    let state_dir = project.path.join(".llm-wiki");
    fs::create_dir_all(&state_dir).map_err(io_error)?;
    atomic_write(
        &state_dir.join("review.json"),
        serde_json::to_vec_pretty(&reviews).unwrap().as_slice(),
    )?;
    Ok(Json(result))
}

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Deserialize)]
struct CreateChatSessionRequest {
    title: Option<String>,
}

async fn chat(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<ChatRequest>,
) -> ApiResult<Json<Value>> {
    get_project(&state, &project_id)?;
    let provider = chat_provider_config(&state)?;
    if input.message.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "MESSAGE_REQUIRED",
            "message is required",
        ));
    }
    let message = ChatMessage {
        id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: input.message,
        created_at: now_secs(),
    };
    let reply = provider_reply(&state, &provider, &[message]).await?;
    Ok(Json(json!({"message": reply})))
}

async fn list_chat_sessions(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    get_project(&state, &project_id)?;
    let mut sessions: Vec<_> = state
        .chat_sessions
        .lock()
        .unwrap()
        .values()
        .filter(|session| session.project_id == project_id)
        .cloned()
        .collect();
    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(Json(json!({
        "items": sessions
            .into_iter()
            .map(|session| json!({
                "id": session.id,
                "title": session.title,
                "createdAt": session.created_at,
                "updatedAt": session.updated_at,
            }))
            .collect::<Vec<_>>()
    })))
}

async fn create_chat_session(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<CreateChatSessionRequest>,
) -> ApiResult<(StatusCode, Json<ChatSession>)> {
    get_project(&state, &project_id)?;
    let now = now_secs();
    let session = ChatSession {
        id: format!("chat_{}", Uuid::new_v4()),
        project_id,
        title: input.title.filter(|title| !title.trim().is_empty()),
        created_at: now,
        updated_at: now,
        messages: Vec::new(),
    };
    {
        let mut sessions = state.chat_sessions.lock().unwrap();
        let mut candidate = sessions.clone();
        candidate.insert(session.id.clone(), session.clone());
        persist_chat_sessions(&state.config, &candidate)?;
        *sessions = candidate;
    }
    Ok((StatusCode::CREATED, Json(session)))
}

async fn get_chat_session(
    State(state): State<AppState>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
) -> ApiResult<Json<ChatSession>> {
    get_project(&state, &project_id)?;
    Ok(Json(get_chat_session_for_project(
        &state,
        &project_id,
        &session_id,
    )?))
}

async fn chat_turn(
    State(state): State<AppState>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
    Json(input): Json<ChatRequest>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    get_project(&state, &project_id)?;
    let provider = chat_provider_config(&state)?;
    let content = input.message.trim();
    if content.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "MESSAGE_REQUIRED",
            "message is required",
        ));
    }
    let user_message = ChatMessage {
        id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: content.to_string(),
        created_at: now_secs(),
    };
    get_chat_session_for_project(&state, &project_id, &session_id)?;
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    {
        let mut turns = state.chat_turns.lock().unwrap();
        if turns.contains_key(&session_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "CHAT_TURN_ACTIVE",
                "a turn is already running for this session",
            ));
        }
        turns.insert(session_id.clone(), cancel_tx);
    }
    let messages = {
        let mut sessions = state.chat_sessions.lock().unwrap();
        let mut candidate = sessions.clone();
        let session = candidate.get_mut(&session_id).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "CHAT_SESSION_NOT_FOUND",
                "chat session not found",
            )
        })?;
        session.messages.push(user_message.clone());
        if session.title.is_none() {
            session.title = Some(user_message.content.chars().take(80).collect());
        }
        session.updated_at = now_secs();
        let messages = session.messages.clone();
        if let Err(error) = persist_chat_sessions(&state.config, &candidate) {
            state.chat_turns.lock().unwrap().remove(&session_id);
            return Err(error);
        }
        *sessions = candidate;
        messages
    };
    let (events, _) = tokio::sync::broadcast::channel::<ChatStreamEvent>(16);
    let updates = BroadcastStream::new(events.subscribe()).filter_map(|event| match event {
        Ok(event) => Some(Ok(Event::default()
            .event(event.event)
            .json_data(event.data)
            .unwrap())),
        Err(_) => None,
    });
    let task_state = state.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            result = provider_reply(&task_state, &provider, &messages) => result.map_err(|error| error.message),
            _ = cancel_rx.changed() => Err("cancelled".to_string()),
        };
        match result {
            Ok(reply) => {
                if !*cancel_rx.borrow() {
                    let persisted = (|| -> ApiResult<bool> {
                        let mut sessions = task_state.chat_sessions.lock().unwrap();
                        let mut candidate = sessions.clone();
                        let Some(session) = candidate.get_mut(&session_id) else {
                            return Ok(false);
                        };
                        session.messages.push(ChatMessage {
                            id: Uuid::new_v4().to_string(),
                            role: "assistant".to_string(),
                            content: reply.clone(),
                            created_at: now_secs(),
                        });
                        session.updated_at = now_secs();
                        persist_chat_sessions(&task_state.config, &candidate)?;
                        *sessions = candidate;
                        Ok(true)
                    })();
                    match persisted {
                        Ok(true) => {
                            let _ = events.send(ChatStreamEvent {
                                event: "delta".to_string(),
                                data: json!({"delta": reply}),
                            });
                        }
                        Ok(false) => {}
                        Err(error) => {
                            let _ = events.send(ChatStreamEvent {
                                event: "error".to_string(),
                                data: json!({"error": error.message}),
                            });
                        }
                    }
                }
            }
            Err(message) if message != "cancelled" => {
                let _ = events.send(ChatStreamEvent {
                    event: "error".to_string(),
                    data: json!({"error": message}),
                });
            }
            Err(_) => {}
        }
        let _ = events.send(ChatStreamEvent {
            event: "done".to_string(),
            data: json!("[DONE]"),
        });
        task_state.chat_turns.lock().unwrap().remove(&session_id);
    });
    Ok(Sse::new(updates).keep_alive(KeepAlive::default()))
}

async fn cancel_chat_turn(
    State(state): State<AppState>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
) -> ApiResult<StatusCode> {
    get_project(&state, &project_id)?;
    let _ = get_chat_session_for_project(&state, &project_id, &session_id)?;
    if let Some(cancel) = state.chat_turns.lock().unwrap().get(&session_id) {
        let _ = cancel.send(true);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn get_chat_session_for_project(
    state: &AppState,
    project_id: &str,
    session_id: &str,
) -> ApiResult<ChatSession> {
    state
        .chat_sessions
        .lock()
        .unwrap()
        .get(session_id)
        .filter(|session| session.project_id == project_id)
        .cloned()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "CHAT_SESSION_NOT_FOUND",
                "chat session not found",
            )
        })
}

fn chat_provider_config(state: &AppState) -> ApiResult<ChatProviderConfig> {
    chat_provider_config_from(state, |name| std::env::var(name).ok())
}

fn chat_provider_config_from<F>(state: &AppState, env: F) -> ApiResult<ChatProviderConfig>
where
    F: Fn(&str) -> Option<String>,
{
    let endpoint = env("LLM_WIKI_LLM_ENDPOINT")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| web_chat_setting(state, "endpoint"))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHAT_NOT_CONFIGURED",
                "LLM endpoint is not configured",
            )
        })?;
    let model = env("LLM_WIKI_LLM_MODEL")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| web_chat_setting(state, "model"))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHAT_NOT_CONFIGURED",
                "LLM model is not configured",
            )
        })?;
    validate_provider_endpoint(&endpoint)?;
    Ok(ChatProviderConfig {
        endpoint,
        model,
        api_key: env("LLM_WIKI_LLM_API_KEY")
            .filter(|value| !value.is_empty())
            .or_else(|| web_chat_setting(state, "apiKey")),
    })
}

fn web_chat_setting(state: &AppState, key: &str) -> Option<String> {
    state
        .settings
        .lock()
        .unwrap()
        .get("webChat")
        .and_then(Value::as_object)
        .and_then(|settings| settings.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn validate_provider_endpoint(endpoint: &str) -> ApiResult<()> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_LLM_ENDPOINT",
            "LLM_WIKI_LLM_ENDPOINT must be an absolute HTTP(S) URL",
        )
    })?;
    let host = url.host_str().unwrap_or_default();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|ip| ip.is_loopback());
    if url.username() != ""
        || url.password().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_LLM_ENDPOINT",
            "LLM endpoint must use HTTPS, except for an explicit loopback HTTP endpoint",
        ));
    }
    Ok(())
}

async fn provider_reply(
    state: &AppState,
    provider: &ChatProviderConfig,
    messages: &[ChatMessage],
) -> ApiResult<String> {
    let mut request = state.client.post(&provider.endpoint).json(&json!({
        "model": provider.model,
        "messages": messages.iter().map(|message| json!({
            "role": message.role,
            "content": message.content,
        })).collect::<Vec<_>>(),
        "stream": false,
    }));
    if let Some(key) = &provider.api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, "LLM_ERROR", error.to_string()))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, "LLM_ERROR", error.to_string()))?;
    if !status.is_success() {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "LLM_ERROR",
            format!("provider returned {status}"),
        ));
    }
    body.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "LLM_INVALID_RESPONSE",
                "provider response did not include choices[0].message.content",
            )
        })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateJobRequest {
    project_id: Option<String>,
    #[serde(alias = "type")]
    kind: String,
}

async fn create_job(
    State(state): State<AppState>,
    Json(input): Json<CreateJobRequest>,
) -> ApiResult<(StatusCode, Json<Job>)> {
    match (input.kind.as_str(), input.project_id.as_deref()) {
        ("index-rebuild", Some(project_id)) => {
            let project = get_project(&state, project_id)?;
            let job = start_index_rebuild(state, project)?;
            Ok((StatusCode::ACCEPTED, Json(job)))
        }
        ("index-rebuild", None) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROJECT_ID_REQUIRED",
            "index-rebuild requires projectId",
        )),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "JOB_TYPE_UNSUPPORTED",
            "unsupported job type",
        )),
    }
}

async fn rebuild_index(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult<(StatusCode, Json<Job>)> {
    let project = get_project(&state, &project_id)?;
    let job = start_index_rebuild(state, project)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

fn start_index_rebuild(state: AppState, project: Project) -> ApiResult<Job> {
    let job = new_job(Some(project.id.clone()), "index-rebuild".to_string());
    record_job(&state, job.clone())?;
    let task_state = state.clone();
    let job_id = job.id.clone();
    tokio::task::spawn_blocking(move || {
        let _ = update_job(
            &task_state,
            &job_id,
            "running",
            Some(0),
            None,
            "Scanning Markdown files",
            None,
        );
        if job_by_id(&task_state, &job_id)
            .ok()
            .is_some_and(|job| job.status == "cancelled")
        {
            return;
        }
        let project_lock = project_write_lock(&task_state, &project.id);
        let result = {
            let _project_guard = project_lock.lock().unwrap();
            rebuild_wiki_index(&project.path)
        };
        match result {
            Ok((pages, groups)) => {
                let _ = update_job(
                    &task_state,
                    &job_id,
                    "completed",
                    Some(pages as u64),
                    Some(pages as u64),
                    &format!("Rebuilt wiki/index.md with {pages} pages in {groups} groups"),
                    None,
                );
            }
            Err(error) => {
                let message = error.message.clone();
                let _ = update_job(
                    &task_state,
                    &job_id,
                    "failed",
                    None,
                    None,
                    &message,
                    Some(message.clone()),
                );
            }
        }
    });
    Ok(job)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobsQuery {
    project_id: Option<String>,
    status: Option<String>,
}

async fn list_jobs(State(state): State<AppState>, Query(query): Query<JobsQuery>) -> Json<Value> {
    let jobs: Vec<_> = state
        .jobs
        .lock()
        .unwrap()
        .values()
        .filter(|job| {
            query
                .project_id
                .as_ref()
                .is_none_or(|id| job.project_id.as_ref() == Some(id))
        })
        .filter(|job| {
            query
                .status
                .as_ref()
                .is_none_or(|status| &job.status == status)
        })
        .cloned()
        .collect();
    Json(json!({"items": jobs}))
}

async fn get_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult<Json<Job>> {
    Ok(Json(job_by_id(&state, &job_id)?))
}

async fn cancel_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult<Json<Job>> {
    update_job(
        &state,
        &job_id,
        "cancelled",
        None,
        None,
        "Cancellation requested",
        None,
    )?;
    Ok(Json(job_by_id(&state, &job_id)?))
}

async fn retry_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult<(StatusCode, Json<Job>)> {
    let old = job_by_id(&state, &job_id)?;
    if old.job_type != "index-rebuild" {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "JOB_TYPE_UNSUPPORTED",
            "this job type cannot be retried",
        ));
    }
    let project_id = old.project_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROJECT_ID_REQUIRED",
            "index-rebuild requires projectId",
        )
    })?;
    let project = get_project(&state, &project_id)?;
    let job = start_index_rebuild(state, project)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn job_events(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    let initial = job_by_id(&state, &job_id)?;
    let id = job_id.clone();
    let updates =
        BroadcastStream::new(state.events.subscribe()).filter_map(move |event| match event {
            Ok(job) if job.id == id => {
                Some(Ok(Event::default().event("job").json_data(job).unwrap()))
            }
            _ => None,
        });
    let first =
        stream::once(async move { Ok(Event::default().event("job").json_data(initial).unwrap()) });
    Ok(Sse::new(first.chain(updates)).keep_alive(KeepAlive::default()))
}

async fn project_events(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    let project_id = query.project_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROJECT_ID_REQUIRED",
            "projectId is required",
        )
    })?;
    get_project(&state, &project_id)?;
    let initial: Vec<_> = state
        .jobs
        .lock()
        .unwrap()
        .values()
        .filter(|job| job.project_id.as_deref() == Some(project_id.as_str()))
        .cloned()
        .collect();
    let filter_id = project_id.clone();
    let updates =
        BroadcastStream::new(state.events.subscribe()).filter_map(move |event| match event {
            Ok(job) if job.project_id.as_deref() == Some(filter_id.as_str()) => {
                Some(Ok(Event::default().event("job").json_data(job).unwrap()))
            }
            _ => None,
        });
    let first = stream::iter(
        initial
            .into_iter()
            .map(|job| Ok(Event::default().event("job").json_data(job).unwrap())),
    );
    Ok(Sse::new(first.chain(updates)).keep_alive(KeepAlive::default()))
}

fn job_by_id(state: &AppState, job_id: &str) -> ApiResult<Job> {
    state
        .jobs
        .lock()
        .unwrap()
        .get(job_id)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "job not found"))
}

fn record_job(state: &AppState, job: Job) -> ApiResult<()> {
    {
        let mut jobs = state.jobs.lock().unwrap();
        let mut candidate = jobs.clone();
        candidate.insert(job.id.clone(), job.clone());
        persist_jobs(&state.config, &candidate)?;
        *jobs = candidate;
    }
    let _ = state.events.send(job);
    Ok(())
}

fn update_job(
    state: &AppState,
    id: &str,
    status: &str,
    current: Option<u64>,
    total: Option<u64>,
    message: &str,
    error: Option<String>,
) -> ApiResult<()> {
    let job = {
        let mut jobs = state.jobs.lock().unwrap();
        let mut candidate = jobs.clone();
        let job = candidate.get_mut(id).ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "job not found")
        })?;
        job.status = status.to_string();
        job.progress = JobProgress {
            current,
            total,
            message: Some(message.to_string()),
        };
        job.error = error;
        job.updated_at = now_secs();
        let job = job.clone();
        persist_jobs(&state.config, &candidate)?;
        *jobs = candidate;
        job
    };
    let _ = state.events.send(job);
    Ok(())
}

async fn static_file(State(state): State<AppState>, request: Request) -> ApiResult<Response> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Not found",
        ));
    }
    let relative = request.uri().path().trim_start_matches('/');
    if relative.starts_with("api/")
        || relative
            .split('/')
            .any(|part| part == ".." || part.starts_with('.'))
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Not found",
        ));
    }
    let path = static_web_path(&state.config.web_root, relative)?;
    let length = fs::metadata(&path).map_err(|_| web_root_not_found())?.len();
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let body = stream_file_body(&path, 0, length, request.method() == Method::HEAD).await?;
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap());
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self'; frame-src 'self'; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    Ok(response)
}

fn canonicalize_web_root(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("resolve web root: {error}"))?
            .join(path)
    };
    let canonical =
        fs::canonicalize(&absolute).map_err(|error| format!("canonicalize web root: {error}"))?;
    if !canonical.is_dir() {
        return Err("web root must be a directory".to_string());
    }
    Ok(canonical)
}

fn static_web_path(web_root: &Path, relative: &str) -> ApiResult<PathBuf> {
    let candidate = web_root.join(relative);
    if !relative.is_empty() && candidate.exists() {
        let canonical = fs::canonicalize(&candidate).map_err(|_| web_root_not_found())?;
        if !canonical.starts_with(web_root) {
            return Err(web_root_not_found());
        }
        if canonical.is_file() {
            return Ok(canonical);
        }
    }
    let canonical =
        fs::canonicalize(web_root.join("index.html")).map_err(|_| web_root_not_found())?;
    if !canonical.starts_with(web_root) || !canonical.is_file() {
        return Err(web_root_not_found());
    }
    Ok(canonical)
}

fn web_root_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "WEB_ROOT_NOT_FOUND",
        "static web asset not found",
    )
}

fn get_project(state: &AppState, id: &str) -> ApiResult<Project> {
    state
        .projects
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "PROJECT_NOT_FOUND",
                "project not found",
            )
        })
}

fn project_write_lock(state: &AppState, project_id: &str) -> Arc<Mutex<()>> {
    let mut locks = state.project_write_locks.lock().unwrap();
    locks
        .entry(project_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn rollback_new_project_directory(root: &Path, error: ApiError) -> ApiError {
    match fs::remove_dir_all(root) {
        Ok(()) => error,
        Err(rollback_error) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "PROJECT_CREATE_ROLLBACK_FAILED",
            format!("{}; rollback failed: {rollback_error}", error.message),
        ),
    }
}

fn load_registry(config: &ServerConfig) -> Result<BTreeMap<String, Project>, String> {
    let registry_path = config.data_dir.join("projects.json");
    let mut projects = BTreeMap::new();
    let registry_exists = registry_path.exists();
    if registry_exists {
        let value: Value =
            serde_json::from_slice(&fs::read(&registry_path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("invalid projects.json: {e}"))?;
        if let Some(items) = value.as_array() {
            for item in items {
                import_project_value(item, config, &mut projects);
            }
        }
    }
    if !registry_exists {
        let app_state = config.app_state.clone().or_else(|| {
            let candidate = config.data_dir.join("app-state.json");
            candidate.exists().then_some(candidate)
        });
        if let Some(app_state) = app_state {
            if let Ok(value) = fs::read(&app_state)
                .and_then(|v| serde_json::from_slice::<Value>(&v).map_err(std::io::Error::other))
            {
                if let Some(registry) = value.get("projectRegistry").and_then(Value::as_object) {
                    for (id, item) in registry {
                        let mut item = item.clone();
                        item["id"] = Value::String(id.clone());
                        import_project_value(&item, config, &mut projects);
                    }
                }
            }
        }
        for item in fs::read_dir(&config.workspace_root).map_err(|e| e.to_string())? {
            let Ok(item) = item else { continue };
            let Ok(metadata) = item.file_type() else {
                continue;
            };
            if !metadata.is_dir() || item.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(path) = fs::canonicalize(item.path()) else {
                continue;
            };
            if projects.values().any(|p| p.path == path) {
                continue;
            }
            let name = item.file_name().to_string_lossy().to_string();
            let id = stable_project_id(&path);
            projects.insert(
                id.clone(),
                Project {
                    id,
                    name,
                    path,
                    created_at: now_secs(),
                },
            );
        }
    }
    persist_registry(config, &projects).map_err(|e| e.message)?;
    Ok(projects)
}

fn import_project_value(
    value: &Value,
    config: &ServerConfig,
    projects: &mut BTreeMap<String, Project>,
) {
    let Some(raw_path) = value.get("path").and_then(Value::as_str) else {
        return;
    };
    let Ok(path) = fs::canonicalize(raw_path) else {
        return;
    };
    if !path.starts_with(&config.workspace_root) || !path.is_dir() {
        return;
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| stable_project_id(&path));
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    projects.insert(
        id.clone(),
        Project {
            id,
            name,
            path,
            created_at: value
                .get("createdAt")
                .and_then(Value::as_u64)
                .unwrap_or_else(now_secs),
        },
    );
}

fn persist_registry(config: &ServerConfig, projects: &BTreeMap<String, Project>) -> ApiResult<()> {
    let records: Vec<_> = projects
        .values()
        .map(|project| RegistryProject {
            id: &project.id,
            name: &project.name,
            path: project.path.to_string_lossy().to_string(),
            created_at: project.created_at,
        })
        .collect();
    let bytes = serde_json::to_vec_pretty(&records).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SERIALIZE_ERROR",
            e.to_string(),
        )
    })?;
    atomic_write_private(&config.data_dir.join("projects.json"), &bytes)
}

fn load_settings(config: &ServerConfig) -> Result<Value, String> {
    let path = config.data_dir.join("settings.json");
    if !path.exists() {
        return Ok(json!({}));
    }
    let settings: Value =
        serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid settings.json: {error}"))?;
    if settings.is_object() {
        ensure_private_file(&path).map_err(|error| error.message)?;
        Ok(settings)
    } else {
        Err("invalid settings.json: top level must be an object".to_string())
    }
}

fn persist_settings(config: &ServerConfig, settings: &Value) -> ApiResult<()> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(|error| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SERIALIZE_ERROR",
            error.to_string(),
        )
    })?;
    atomic_write_private(&config.data_dir.join("settings.json"), &bytes)
}

fn load_chat_sessions(config: &ServerConfig) -> Result<BTreeMap<String, ChatSession>, String> {
    let path = config.data_dir.join("chat-sessions.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let sessions: Vec<ChatSession> =
        serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid chat-sessions.json: {error}"))?;
    ensure_private_file(&path).map_err(|error| error.message)?;
    Ok(sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect())
}

fn persist_chat_sessions(
    config: &ServerConfig,
    sessions: &BTreeMap<String, ChatSession>,
) -> ApiResult<()> {
    let bytes =
        serde_json::to_vec_pretty(&sessions.values().collect::<Vec<_>>()).map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SERIALIZE_ERROR",
                error.to_string(),
            )
        })?;
    atomic_write_private(&config.data_dir.join("chat-sessions.json"), &bytes)
}

fn merge_json(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    target.remove(&key);
                } else {
                    merge_json(target.entry(key).or_insert(Value::Null), value);
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

fn redact_settings(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(redact_settings).collect()),
        Value::Object(items) => Value::Object(
            items
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_key(key) {
                        json!({"configured": !value.is_null() && value != ""})
                    } else {
                        redact_settings(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        value => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key
        .chars()
        .filter(|character| *character != '_' && *character != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    ["apikey", "token", "secret", "password", "credential"]
        .iter()
        .any(|needle| key.contains(needle))
}

fn load_jobs(config: &ServerConfig) -> Result<BTreeMap<String, Job>, String> {
    let path = config.data_dir.join("jobs.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let mut jobs: Vec<Job> =
        serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid jobs.json: {error}"))?;
    ensure_private_file(&path).map_err(|error| error.message)?;
    let mut interrupted = false;
    for job in &mut jobs {
        if job.status == "running" {
            job.status = "interrupted".to_string();
            job.error = Some("Server restarted before this job completed".to_string());
            job.progress.message = Some("Interrupted after server restart".to_string());
            job.updated_at = now_secs();
            interrupted = true;
        }
    }
    let jobs: BTreeMap<_, _> = jobs.into_iter().map(|job| (job.id.clone(), job)).collect();
    if interrupted {
        persist_jobs(config, &jobs).map_err(|error| error.message)?;
    }
    Ok(jobs)
}

fn persist_jobs(config: &ServerConfig, jobs: &BTreeMap<String, Job>) -> ApiResult<()> {
    let bytes = serde_json::to_vec_pretty(&jobs.values().collect::<Vec<_>>()).map_err(|error| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SERIALIZE_ERROR",
            error.to_string(),
        )
    })?;
    atomic_write_private(&config.data_dir.join("jobs.json"), &bytes)
}

fn create_project_layout(root: &Path) -> ApiResult<()> {
    for directory in [
        "raw/sources",
        "raw/assets",
        "wiki/entities",
        "wiki/concepts",
        "wiki/sources",
        "wiki/queries",
        "wiki/comparisons",
        "wiki/synthesis",
    ] {
        fs::create_dir_all(root.join(directory)).map_err(io_error)?;
    }
    fs::create_dir_all(root.join(".llm-wiki")).map_err(io_error)?;
    atomic_write(
        &root.join("purpose.md"),
        b"# Project Purpose\n\nDescribe the purpose, scope, and key questions for this knowledge base.\n",
    )?;
    atomic_write(
        &root.join("schema.md"),
        b"# Wiki Schema\n\n- `wiki/entities/`: named entities\n- `wiki/concepts/`: concepts and methods\n- `wiki/sources/`: source summaries\n- `wiki/queries/`: research questions\n- `wiki/comparisons/`: comparisons\n- `wiki/synthesis/`: cross-cutting conclusions\n",
    )?;
    atomic_write(
        &root.join("wiki/index.md"),
        b"# Wiki Index\n\n## Entities\n\n## Concepts\n\n## Sources\n\n## Queries\n\n## Comparisons\n\n## Synthesis\n",
    )?;
    atomic_write(
        &root.join("wiki/log.md"),
        b"# Research Log\n\n- Project created from LLM Wiki Web.\n",
    )?;
    atomic_write(
        &root.join("wiki/overview.md"),
        b"---\ntype: overview\ntitle: Project Overview\ntags: []\nrelated: []\n---\n\n# Overview\n\nDescribe the current state of this knowledge base.\n",
    )?;
    atomic_write(&root.join("README.md"), b"# LLM Wiki\n")?;
    Ok(())
}

fn rebuild_wiki_index(project_root: &Path) -> ApiResult<(usize, usize)> {
    let wiki_root = project_root.join("wiki");
    let mut groups = BTreeMap::<String, Vec<(String, String)>>::new();
    for entry in WalkDir::new(&wiki_root).follow_links(false) {
        let entry = entry.map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INDEX_SCAN_FAILED",
                error.to_string(),
            )
        })?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("md")
        {
            continue;
        }
        let stem = entry
            .path()
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if matches!(
            stem.to_ascii_lowercase().as_str(),
            "index" | "overview" | "log"
        ) {
            continue;
        }
        let content = fs::read_to_string(entry.path()).map_err(io_error)?;
        let page_type = frontmatter_value(&content, "type").unwrap_or_else(|| "other".into());
        let title = frontmatter_value(&content, "title")
            .or_else(|| markdown_title(&content))
            .unwrap_or_else(|| stem.to_string());
        let target = entry
            .path()
            .strip_prefix(&wiki_root)
            .map_err(|_| path_escape())?
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        groups.entry(page_type).or_default().push((target, title));
    }
    for pages in groups.values_mut() {
        pages.sort_by(|left, right| left.1.to_lowercase().cmp(&right.1.to_lowercase()));
    }
    let page_count = groups.values().map(Vec::len).sum();
    let mut output = String::from("# Wiki Index\n\n");
    for (page_type, pages) in &groups {
        output.push_str(&format!("## {page_type}\n\n"));
        for (target, title) in pages {
            output.push_str(&format!("- [[{target}|{title}]]\n"));
        }
        output.push('\n');
    }
    atomic_write(&wiki_root.join("index.md"), output.as_bytes())?;
    Ok((page_count, groups.len()))
}

fn frontmatter_value(content: &str, key: &str) -> Option<String> {
    let normalized = content.replace("\r\n", "\n");
    let body = normalized.strip_prefix("---\n")?.split_once("\n---")?.0;
    body.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == key).then(|| value.trim().trim_matches(['"', '\'']).to_string())
        })
        .filter(|value| !value.is_empty())
}

fn workspace_relative(root: &Path, relative: &str) -> ApiResult<PathBuf> {
    validate_relative(relative, false)?;
    let path = root.join(relative);
    let canonical = fs::canonicalize(&path).map_err(not_found_io)?;
    if !canonical.starts_with(root) {
        return Err(path_escape());
    }
    Ok(canonical)
}

fn safe_project_path(root: &Path, relative: &str, allow_empty: bool) -> ApiResult<PathBuf> {
    validate_relative(relative, allow_empty)?;
    let mut current = root.to_path_buf();
    if relative.is_empty() {
        return Ok(current);
    }
    for component in Path::new(relative).components() {
        let Component::Normal(part) = component else {
            return Err(path_escape());
        };
        current.push(part);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(path_escape());
            }
        }
    }
    let mut ancestor = current.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(path_escape)?;
    }
    let canonical = fs::canonicalize(ancestor).map_err(io_error)?;
    if !canonical.starts_with(root) {
        return Err(path_escape());
    }
    Ok(current)
}

fn collect_file_tree(root: &Path, directory: &Path) -> ApiResult<Vec<FileTreeNode>> {
    let mut nodes = Vec::new();
    for item in fs::read_dir(directory).map_err(io_error)? {
        let item = item.map_err(io_error)?;
        let name = item.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let relative = relative_string(root, &path)?;
        let is_dir = metadata.is_dir();
        nodes.push(FileTreeNode {
            name,
            path: relative,
            is_dir,
            children: is_dir.then(|| collect_file_tree(root, &path)).transpose()?,
            mime_type: (!is_dir).then(|| {
                mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .essence_str()
                    .to_string()
            }),
            size: (!is_dir).then_some(metadata.len()),
        });
    }
    nodes.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(nodes)
}

fn validate_relative(relative: &str, allow_empty: bool) -> ApiResult<()> {
    if relative.contains('\0')
        || (!allow_empty && relative.trim().is_empty())
        || Path::new(relative).is_absolute()
    {
        return Err(path_escape());
    }
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) if !part.to_string_lossy().starts_with('.') => {}
            _ => return Err(path_escape()),
        }
    }
    Ok(())
}

fn path_escape() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "INVALID_PATH",
        "path must remain inside the project and may not contain hidden or symlink components",
    )
}
fn required_path(path: Option<String>) -> ApiResult<String> {
    path.filter(|v| !v.trim().is_empty())
        .ok_or_else(path_escape)
}
fn validate_project_directory(value: &str) -> ApiResult<()> {
    if value.is_empty()
        || value.starts_with('.')
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_DIRECTORY_NAME",
            "directoryName may contain only letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}
fn validate_file_name(value: &str) -> ApiResult<()> {
    if value.is_empty() || value.starts_with('.') || value.contains(['/', '\\', '\0']) {
        return Err(path_escape());
    }
    Ok(())
}

fn is_active_content(mime: &mime::Mime, path: &Path) -> bool {
    matches!(
        mime.essence_str(),
        "text/html"
            | "application/xhtml+xml"
            | "image/svg+xml"
            | "application/xml"
            | "text/xml"
            | "application/javascript"
            | "text/javascript"
            | "application/ecmascript"
            | "text/ecmascript"
            | "application/x-javascript"
            | "application/hta"
    ) || matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "htm"
                | "html"
                | "xhtml"
                | "svg"
                | "svgz"
                | "xml"
                | "xsl"
                | "xslt"
                | "js"
                | "mjs"
                | "cjs"
                | "jsx"
                | "hta"
        )
    )
}

async fn stream_file_body(path: &Path, start: u64, length: u64, head: bool) -> ApiResult<Body> {
    if head || length == 0 {
        return Ok(Body::empty());
    }
    let mut file = tokio::fs::File::open(path).await.map_err(not_found_io)?;
    file.seek(SeekFrom::Start(start)).await.map_err(io_error)?;
    Ok(Body::from_stream(ReaderStream::with_capacity(
        file.take(length),
        64 * 1024,
    )))
}

fn slugify(name: &str) -> String {
    let value = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    value
        .split('-')
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
fn atomic_write(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    atomic_write_with_permissions(path, bytes, false)
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    atomic_write_with_permissions(path, bytes, true)
}

fn atomic_write_with_permissions(path: &Path, bytes: &[u8], private: bool) -> ApiResult<()> {
    let parent = path.parent().ok_or_else(path_escape)?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let temp = parent.join(format!(".llm-wiki-write-{}", Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(io_error)
}

fn ensure_private_file(path: &Path) -> ApiResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
    }
    Ok(())
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn quoted_etag(value: &str) -> ApiResult<HeaderValue> {
    HeaderValue::from_str(&format!("\"{value}\"")).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "HEADER_ERROR",
            e.to_string(),
        )
    })
}
fn supplied_revision(
    headers: &HeaderMap,
    body_revision: Option<String>,
) -> ApiResult<Option<String>> {
    let header_revision = headers
        .get(IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(unquote_etag);
    if let (Some(header), Some(body)) = (&header_revision, &body_revision) {
        if header != body {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "REVISION_MISMATCH",
                "If-Match and body revision must match when both are supplied",
            ));
        }
    }
    Ok(header_revision.or(body_revision))
}
fn merge_revision(left: Option<String>, right: Option<String>) -> ApiResult<Option<String>> {
    if let (Some(left), Some(right)) = (&left, &right) {
        if left != right {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "REVISION_MISMATCH",
                "query and body revisions must match when both are supplied",
            ));
        }
    }
    Ok(left.or(right))
}
fn unquote_etag(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("W/")
        .trim_matches('"')
        .to_string()
}
fn parse_range(value: Option<&str>, size: u64) -> ApiResult<(StatusCode, u64, u64)> {
    if size == 0 {
        return Ok((StatusCode::OK, 0, 0));
    }
    let Some(value) = value else {
        return Ok((StatusCode::OK, 0, size - 1));
    };
    let spec = value.strip_prefix("bytes=").ok_or_else(|| {
        ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "INVALID_RANGE",
            "invalid Range header",
        )
    })?;
    if spec.contains(',') {
        return Err(ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "INVALID_RANGE",
            "multiple ranges are not supported",
        ));
    }
    let (start, end) = spec.split_once('-').ok_or_else(|| {
        ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "INVALID_RANGE",
            "invalid Range header",
        )
    })?;
    let (start, end) = if start.is_empty() {
        let suffix = end
            .parse::<u64>()
            .map_err(|_| {
                ApiError::new(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "INVALID_RANGE",
                    "invalid Range header",
                )
            })?
            .min(size);
        (size - suffix, size - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| {
            ApiError::new(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "INVALID_RANGE",
                "invalid Range header",
            )
        })?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        "INVALID_RANGE",
                        "invalid Range header",
                    )
                })?
                .min(size - 1)
        };
        (start, end)
    };
    if start >= size || start > end {
        return Err(ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "INVALID_RANGE",
            "range is outside the file",
        ));
    }
    Ok((StatusCode::PARTIAL_CONTENT, start, end))
}
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.to_string())
        })
}
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        diff |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    diff == 0
}
fn hash_string(value: &str) -> String {
    revision(value.as_bytes())
}
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn modified_secs(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|v| v.as_secs())
        .unwrap_or(0)
}
fn stable_project_id(path: &Path) -> String {
    format!(
        "project-{}",
        &revision(path.to_string_lossy().as_bytes())[..16]
    )
}
fn relative_string(root: &Path, path: &Path) -> ApiResult<String> {
    path.strip_prefix(root)
        .map(|v| v.to_string_lossy().replace('\\', "/"))
        .map_err(|_| path_escape())
}
fn is_hidden_relative(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).ok().is_none_or(|v| {
        v.components().any(
            |c| matches!(c, Component::Normal(part) if part.to_string_lossy().starts_with('.')),
        )
    })
}
fn markdown_title(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}
fn search_snippet(content: &str, terms: &[String]) -> String {
    content
        .lines()
        .find(|line| {
            let lower = line.to_lowercase();
            terms.iter().any(|term| lower.contains(term))
        })
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}
fn wiki_links(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("]]") else { break };
        let link = rest[..end].split('|').next().unwrap_or("").trim();
        if !link.is_empty() {
            out.push(link.to_string());
        }
        rest = &rest[end + 2..];
    }
    out
}
fn resolve_graph_link(link: &str, ids: &[String]) -> Option<String> {
    let normalized = link.trim_end_matches(".md").replace('\\', "/");
    ids.iter()
        .find(|id| {
            id.eq_ignore_ascii_case(&normalized)
                || id
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&normalized.replace(' ', "-")))
        })
        .cloned()
}
fn graph_type(path: &str) -> String {
    path.split('/')
        .nth(1)
        .unwrap_or("other")
        .trim_end_matches('s')
        .to_string()
}
fn read_reviews(root: &Path) -> ApiResult<Vec<Value>> {
    let path = root.join(".llm-wiki/review.json");
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INVALID_REVIEW_STATE",
                e.to_string(),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_error(e)),
    }
}
fn new_job(project_id: Option<String>, job_type: String) -> Job {
    let now = now_secs();
    Job {
        id: format!("job_{}", Uuid::new_v4()),
        project_id,
        job_type,
        status: "queued".to_string(),
        progress: JobProgress {
            current: Some(0),
            total: None,
            message: Some("Queued".to_string()),
        },
        created_at: now,
        updated_at: now,
        error: None,
    }
}
fn io_error(error: std::io::Error) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "IO_ERROR",
        error.to_string(),
    )
}
fn not_found_io(error: std::io::Error) -> ApiError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ApiError::new(StatusCode::NOT_FOUND, "FILE_NOT_FOUND", "file not found")
    } else {
        io_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::io::Read;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/web-server-tests")
            .join(Uuid::new_v4().to_string());
        let workspace = base.join("workspace");
        let data = base.join("data");
        let web = base.join("web");
        fs::create_dir_all(&web).unwrap();
        fs::write(web.join("index.html"), "ok").unwrap();
        AppState::new(ServerConfig {
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            workspace_root: workspace,
            data_dir: data,
            web_root: web,
            bootstrap_token: "secret".to_string(),
            allow_insecure_remote: false,
            secure_cookie: false,
            app_state: None,
        })
        .unwrap()
    }

    fn seed_project(state: &AppState, id: &str) -> PathBuf {
        let root = state.config.workspace_root.join(id);
        create_project_layout(&root).unwrap();
        let project = Project {
            id: id.to_string(),
            name: id.to_string(),
            path: fs::canonicalize(&root).unwrap(),
            created_at: now_secs(),
        };
        state
            .projects
            .lock()
            .unwrap()
            .insert(id.to_string(), project);
        root
    }

    #[test]
    fn path_escape_and_symlink_are_rejected() {
        let state = test_state();
        let root = state.config.workspace_root.join("p");
        fs::create_dir_all(&root).unwrap();
        assert!(safe_project_path(&root, "../secret", false).is_err());
        assert!(safe_project_path(&root, "/etc/passwd", false).is_err());
        assert!(safe_project_path(&root, ".llm-wiki/review.json", false).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
            assert!(safe_project_path(&root, "escape/passwd", false).is_err());
        }
        let _ = fs::remove_dir_all(state.config.data_dir.parent().unwrap());
    }

    #[tokio::test]
    async fn protected_api_rejects_unauthorized_request() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v2/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn write_rejects_revision_conflict() {
        let state = test_state();
        let root = state.config.workspace_root.join("project");
        create_project_layout(&root).unwrap();
        let project = Project {
            id: "p1".to_string(),
            name: "P1".to_string(),
            path: fs::canonicalize(&root).unwrap(),
            created_at: now_secs(),
        };
        state
            .projects
            .lock()
            .unwrap()
            .insert(project.id.clone(), project);
        fs::write(root.join("wiki/page.md"), "old").unwrap();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v2/projects/p1/files/content?path=wiki/page.md")
                    .header("authorization", "Bearer secret")
                    .header(IF_MATCH, "stale")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"content":"new"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("FILE_REVISION_CONFLICT"));
        assert_eq!(
            fs::read_to_string(root.join("wiki/page.md")).unwrap(),
            "old"
        );
    }

    #[tokio::test]
    async fn wildcard_revision_creates_a_new_text_file() {
        let state = test_state();
        let root = seed_project(&state, "p1");
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v2/projects/p1/files/content?path=wiki/new-page.md")
                    .header("authorization", "Bearer secret")
                    .header(IF_MATCH, "*")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"content":"new page"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            fs::read_to_string(root.join("wiki/new-page.md")).unwrap(),
            "new page"
        );
    }

    #[test]
    fn non_loopback_plaintext_requires_explicit_opt_in() {
        let args = [
            "llm-wiki-server",
            "--host",
            "0.0.0.0",
            "--workspace-root",
            "workspace",
            "--data-dir",
            "data",
            "--token",
            "0123456789abcdef0123456789abcdef",
        ];
        assert!(ServerConfig::from_args(args).is_err());
    }

    #[test]
    fn server_config_requires_a_32_character_bootstrap_token() {
        let short = ServerConfig::from_args([
            "llm-wiki-server",
            "--workspace-root",
            "workspace",
            "--data-dir",
            "data",
            "--token",
            "too-short",
        ]);
        assert!(short
            .unwrap_err()
            .contains("must be at least 32 characters"));

        let valid = ServerConfig::from_args([
            "llm-wiki-server",
            "--workspace-root",
            "workspace",
            "--data-dir",
            "data",
            "--token",
            "0123456789abcdef0123456789abcdef",
        ]);
        assert!(valid.is_ok());
    }

    #[tokio::test]
    async fn session_write_requires_origin_and_csrf() {
        let state = test_state();
        let login = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/auth/login")
                    .header(HOST, "localhost")
                    .header(ORIGIN, "http://localhost")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"token":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects")
                    .header(COOKIE, cookie)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Project"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn csrf_refresh_supports_get_without_a_csrf_token() {
        let state = test_state();
        let login = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/auth/login")
                    .header(HOST, "localhost")
                    .header(ORIGIN, "http://localhost")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"token":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = login
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v2/auth/csrf")
                    .header(COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn active_assets_are_download_only_and_head_has_no_body() {
        let state = test_state();
        let root = seed_project(&state, "p1");
        let asset = root.join("raw/assets/untrusted.svg");
        fs::write(&asset, "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").unwrap();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v2/projects/p1/assets/raw/assets/untrusted.svg")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("attachment"));
        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            "sandbox"
        );

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v2/projects/p1/assets/raw/assets/untrusted.svg")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_LENGTH).unwrap(),
            &fs::metadata(asset).unwrap().len().to_string()
        );
        assert!(to_bytes(response.into_body(), 1).await.unwrap().is_empty());

        fs::write(root.join("raw/assets/报告.pdf"), b"pdf").unwrap();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v2/projects/p1/assets?path=raw%2Fassets%2F%E6%8A%A5%E5%91%8A.pdf&download=true")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let disposition = response
            .headers()
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(disposition.contains("filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf"));
    }

    #[tokio::test]
    async fn registry_persistence_failure_does_not_mutate_memory_or_leave_created_project() {
        let state = test_state();
        let registry = state.config.data_dir.join("projects.json");
        fs::remove_file(&registry).unwrap();
        fs::create_dir(&registry).unwrap();

        let created = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"name":"Created","directoryName":"created"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!state.config.workspace_root.join("created").exists());
        assert!(state.projects.lock().unwrap().is_empty());

        fs::create_dir(state.config.workspace_root.join("registered")).unwrap();
        let registered = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/register")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"relativePath":"registered"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(registered.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(state.projects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn project_rename_and_unregister_persist_without_deleting_files() {
        let state = test_state();
        let root = seed_project(&state, "p1");
        persist_registry(&state.config, &state.projects.lock().unwrap()).unwrap();
        let app = router(state.clone());

        let renamed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v2/projects/p1")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Renamed project"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(renamed.status(), StatusCode::OK);
        assert_eq!(
            state.projects.lock().unwrap().get("p1").unwrap().name,
            "Renamed project"
        );
        let restored = AppState::new((*state.config).clone()).unwrap();
        assert_eq!(
            restored.projects.lock().unwrap().get("p1").unwrap().name,
            "Renamed project"
        );

        let deleted = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v2/projects/p1")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert!(root.is_dir());
        assert!(root.join("wiki/index.md").is_file());
        assert!(!state.projects.lock().unwrap().contains_key("p1"));
        let restored = AppState::new((*state.config).clone()).unwrap();
        assert!(!restored.projects.lock().unwrap().contains_key("p1"));
    }

    #[tokio::test]
    async fn project_rename_and_unregister_do_not_mutate_memory_when_persistence_fails() {
        let state = test_state();
        seed_project(&state, "p1");
        persist_registry(&state.config, &state.projects.lock().unwrap()).unwrap();
        let registry = state.config.data_dir.join("projects.json");
        fs::remove_file(&registry).unwrap();
        fs::create_dir(&registry).unwrap();
        let app = router(state.clone());

        let renamed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v2/projects/p1")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Should not persist"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(renamed.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(state.projects.lock().unwrap().get("p1").unwrap().name, "p1");

        let deleted = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v2/projects/p1")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(state.projects.lock().unwrap().contains_key("p1"));
    }

    #[tokio::test]
    async fn move_rejects_unsafe_or_existing_targets_and_keeps_files_in_project() {
        let state = test_state();
        let root = seed_project(&state, "p1");
        fs::write(root.join("wiki/source.md"), "source").unwrap();
        fs::write(root.join("wiki/existing.md"), "existing").unwrap();
        let app = router(state);

        let moved = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/p1/files/move")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"sourcePath":"wiki/source.md","targetPath":"wiki/moved.md"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(moved.status(), StatusCode::OK);
        assert!(!root.join("wiki/source.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("wiki/moved.md")).unwrap(),
            "source"
        );

        let existing = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/p1/files/move")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"sourcePath":"wiki/moved.md","targetPath":"wiki/existing.md"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(existing.status(), StatusCode::CONFLICT);

        let unsafe_target = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/p1/files/move")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"sourcePath":"wiki/moved.md","targetPath":"../outside.md"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unsafe_target.status(), StatusCode::BAD_REQUEST);
        assert!(root.join("wiki/moved.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn startup_canonicalizes_symlinked_web_root() {
        use std::os::unix::fs::symlink;

        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/web-server-tests")
            .join(Uuid::new_v4().to_string());
        let web = base.join("web");
        fs::create_dir_all(&web).unwrap();
        fs::write(web.join("index.html"), "ok").unwrap();
        symlink(&web, base.join("web-link")).unwrap();
        let state = AppState::new(ServerConfig {
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            workspace_root: base.join("workspace"),
            data_dir: base.join("data"),
            web_root: base.join("web-link"),
            bootstrap_token: "secret".to_string(),
            allow_insecure_remote: false,
            secure_cookie: false,
            app_state: None,
        })
        .unwrap();
        assert_eq!(state.config.web_root, fs::canonicalize(web).unwrap());
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn static_file_symlink_outside_web_root_is_rejected() {
        use std::os::unix::fs::symlink;

        let state = test_state();
        let outside = state.config.data_dir.parent().unwrap().join("outside.js");
        fs::write(&outside, "alert('outside')").unwrap();
        symlink(&outside, state.config.web_root.join("outside.js")).unwrap();

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/outside.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn frontend_file_routes_support_tree_body_revision_and_delete_revision() {
        let state = test_state();
        let root = seed_project(&state, "p1");
        fs::write(root.join("wiki/page.md"), "old").unwrap();

        let tree = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v2/projects/p1/tree")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let tree_body = to_bytes(tree.into_body(), 1024 * 1024).await.unwrap();
        let tree: Value = serde_json::from_slice(&tree_body).unwrap();
        assert_eq!(
            tree.pointer("/tree/1/path").and_then(Value::as_str),
            Some("wiki")
        );

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v2/projects/p1/files/content?path=wiki/page.md")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"content":"new","revision":"{}"}}"#,
                        revision(b"old")
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let saved: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved.get("content").and_then(Value::as_str), Some("new"));
        let revision = saved.get("revision").and_then(Value::as_str).unwrap();

        let deleted = router(state)
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v2/projects/p1/files?path=wiki/page.md")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(r#"{{"revision":"{revision}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn multipart_upload_and_query_asset_download_are_supported() {
        let state = test_state();
        let _root = seed_project(&state, "p1");
        let boundary = "test-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"asset.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
        );
        let upload = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/p1/uploads?path=raw/assets")
                    .header("authorization", "Bearer secret")
                    .header(
                        CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(upload.status(), StatusCode::CREATED);

        let asset = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v2/projects/p1/assets?path=raw%2Fassets%2Fasset.txt&download=true")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(asset.status(), StatusCode::OK);
        assert!(asset
            .headers()
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("attachment"));
        assert_eq!(to_bytes(asset.into_body(), 1024).await.unwrap(), "hello");
        let wildcard = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v2/projects/p1/assets/raw/assets/asset.txt")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wildcard.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn settings_reviews_and_chat_sessions_follow_client_shapes() {
        let state = test_state();
        let root = seed_project(&state, "p1");
        fs::create_dir_all(root.join(".llm-wiki")).unwrap();
        fs::write(
            root.join(".llm-wiki/review.json"),
            r#"[{"id":"r1","title":"Review"}]"#,
        )
        .unwrap();
        let app = router(state.clone());

        let settings = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v2/settings")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"nested":{"api-key":"private","enabled":true},"webChat":{"endpoint":"http://127.0.0.1:8080/v1/chat/completions","model":"settings-model","apiKey":"chat-private"}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings_body = to_bytes(settings.into_body(), 1024 * 1024).await.unwrap();
        assert!(!String::from_utf8_lossy(&settings_body).contains("private"));
        assert_eq!(
            serde_json::from_slice::<Value>(&settings_body)
                .unwrap()
                .pointer("/nested/api-key/configured")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&settings_body)
                .unwrap()
                .pointer("/webChat/apiKey/configured")
                .and_then(Value::as_bool),
            Some(true)
        );

        let review = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v2/projects/p1/reviews/r1")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"status":"resolved","action":"accept"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let review_body: Value =
            serde_json::from_slice(&to_bytes(review.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(
            review_body.get("status").and_then(Value::as_str),
            Some("resolved")
        );
        assert_eq!(
            review_body.get("action").and_then(Value::as_str),
            Some("accept")
        );
        assert_eq!(
            review_body.get("resolved").and_then(Value::as_bool),
            Some(true)
        );

        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/p1/chat/sessions")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"title":"Chat"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created: Value =
            serde_json::from_slice(&to_bytes(created.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        let id = created.get("id").and_then(Value::as_str).unwrap();
        let detail = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v2/projects/p1/chat/sessions/{id}"))
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
    }

    #[test]
    fn chat_provider_uses_web_chat_settings_with_environment_precedence() {
        let state = test_state();
        *state.settings.lock().unwrap() = json!({
            "webChat": {
                "endpoint": "http://127.0.0.1:8080/v1/chat/completions",
                "model": "settings-model",
                "apiKey": "settings-key",
            }
        });
        let settings = chat_provider_config_from(&state, |_| None).unwrap();
        assert_eq!(
            settings.endpoint,
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(settings.model, "settings-model");
        assert_eq!(settings.api_key.as_deref(), Some("settings-key"));

        let environment = chat_provider_config_from(&state, |name| match name {
            "LLM_WIKI_LLM_ENDPOINT" => {
                Some("http://127.0.0.1:8081/v1/chat/completions".to_string())
            }
            "LLM_WIKI_LLM_MODEL" => Some("environment-model".to_string()),
            "LLM_WIKI_LLM_API_KEY" => Some("environment-key".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            environment.endpoint,
            "http://127.0.0.1:8081/v1/chat/completions"
        );
        assert_eq!(environment.model, "environment-model");
        assert_eq!(environment.api_key.as_deref(), Some("environment-key"));

        *state.settings.lock().unwrap() = json!({
            "webChat": {
                "endpoint": "http://example.test/v1/chat/completions",
                "model": "settings-model",
            }
        });
        assert_eq!(
            chat_provider_config_from(&state, |_| None)
                .err()
                .unwrap()
                .code,
            "INVALID_LLM_ENDPOINT"
        );
    }

    #[tokio::test]
    async fn chat_sessions_are_private_and_restore_after_restart() {
        let state = test_state();
        seed_project(&state, "p1");
        persist_registry(&state.config, &state.projects.lock().unwrap()).unwrap();
        let created = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v2/projects/p1/chat/sessions")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"title":"Persisted chat"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created: Value =
            serde_json::from_slice(&to_bytes(created.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        let id = created.get("id").and_then(Value::as_str).unwrap();
        let restored = AppState::new((*state.config).clone()).unwrap();
        let session = restored
            .chat_sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap();
        assert_eq!(session.project_id, "p1");
        assert_eq!(session.title.as_deref(), Some("Persisted chat"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(state.config.data_dir.join("chat-sessions.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn control_plane_json_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let state = test_state();
        seed_project(&state, "p1");
        persist_registry(&state.config, &state.projects.lock().unwrap()).unwrap();
        persist_settings(&state.config, &json!({"apiKey": "secret"})).unwrap();
        persist_jobs(&state.config, &BTreeMap::new()).unwrap();
        persist_chat_sessions(&state.config, &BTreeMap::new()).unwrap();

        for name in [
            "projects.json",
            "settings.json",
            "jobs.json",
            "chat-sessions.json",
        ] {
            assert_eq!(
                fs::metadata(state.config.data_dir.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{name} must be private"
            );
        }
    }

    #[tokio::test]
    async fn static_responses_include_csp_and_cache_headers() {
        let response = router(test_state())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("content-security-policy"));
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-cache");
    }

    #[tokio::test]
    async fn settings_persistence_failure_does_not_mutate_memory() {
        let state = test_state();
        fs::create_dir(state.config.data_dir.join("settings.json")).unwrap();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v2/settings")
                    .header("authorization", "Bearer secret")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"ui":{"language":"zh"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(*state.settings.lock().unwrap(), json!({}));
    }

    #[test]
    fn job_persistence_failure_does_not_mutate_memory() {
        let state = test_state();
        fs::create_dir(state.config.data_dir.join("jobs.json")).unwrap();
        let job = new_job(Some("p1".to_string()), "index-rebuild".to_string());
        assert!(record_job(&state, job.clone()).is_err());
        assert!(!state.jobs.lock().unwrap().contains_key(&job.id));
    }

    #[tokio::test]
    async fn jobs_persist_client_shape_and_running_jobs_become_interrupted() {
        let state = test_state();
        seed_project(&state, "p1");
        let job = new_job(Some("p1".to_string()), "index-rebuild".to_string());
        let id = job.id.clone();
        record_job(&state, job.clone()).unwrap();
        assert_eq!(job.job_type, "index-rebuild");
        update_job(&state, &id, "running", Some(1), Some(2), "Working", None).unwrap();
        let restored = AppState::new((*state.config).clone()).unwrap();
        let restored_job = restored.jobs.lock().unwrap().get(&id).cloned().unwrap();
        assert_eq!(restored_job.status, "interrupted");
        assert!(restored_job.error.is_some());
    }

    #[tokio::test]
    async fn openai_compatible_provider_response_is_consumed() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 45\r\nConnection: close\r\n\r\n{\"choices\":[{\"message\":{\"content\":\"hello\"}}]}")
                .unwrap();
        });
        let state = test_state();
        let result = provider_reply(
            &state,
            &ChatProviderConfig {
                endpoint,
                model: "test-model".to_string(),
                api_key: None,
            },
            &[ChatMessage {
                id: "m1".to_string(),
                role: "user".to_string(),
                content: "hi".to_string(),
                created_at: now_secs(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(result, "hello");
    }
}
