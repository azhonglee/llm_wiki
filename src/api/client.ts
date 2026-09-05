import type {
  ApiErrorResponse,
  ChatSession,
  ChatSessionDetail,
  ChatSessionListResponse,
  CreateDirectoryInput,
  CreateDirectoryResponse,
  CreateProjectInput,
  CsrfToken,
  FileContent,
  FileTreeNode,
  FileTreeNodeResponse,
  FileTreeResponse,
  GraphData,
  Job,
  JobListResponse,
  Project,
  ProjectListResponse,
  RegisterProjectInput,
  ReviewItem,
  ReviewListResponse,
  SaveFileInput,
  SearchResponse,
  SearchResult,
  Session,
  Settings,
  SseEvent,
  MoveFileInput,
  UpdateProjectInput,
  UploadResponse,
  WebChatSettingsInput,
} from "./contracts";

export interface ApiClientOptions {
  baseUrl?: string;
  fetch?: typeof fetch;
}

export interface RequestOptions {
  signal?: AbortSignal;
  headers?: HeadersInit;
}

export interface UploadOptions extends RequestOptions {
  path?: string;
}

export interface SseHandlers {
  onEvent: (event: SseEvent) => void;
  onError?: (error: Error) => void;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly details?: unknown;

  constructor(
    status: number,
    message: string,
    options: { code?: string; requestId?: string; details?: unknown } = {},
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.details = options.details;
  }
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function joinUrl(baseUrl: string, path: string): string {
  return `${baseUrl.replace(/\/$/, "")}/${path.replace(/^\//, "")}`;
}

function addQuery(
  path: string,
  query: Record<string, string | number | boolean | undefined>,
): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined) params.set(key, String(value));
  }
  const encoded = params.toString();
  return encoded ? `${path}?${encoded}` : path;
}

function listFrom<T>(
  value:
    | T[]
    | {
        items?: T[];
        projects?: T[];
        tree?: T[];
        entries?: T[];
        results?: T[];
        reviews?: T[];
        sessions?: T[];
        jobs?: T[];
      },
): T[] {
  if (Array.isArray(value)) return value;
  return (
    value.items ??
    value.projects ??
    value.tree ??
    value.entries ??
    value.results ??
    value.reviews ??
    value.sessions ??
    value.jobs ??
    []
  );
}

function normalizeFileTreeNode(node: FileTreeNodeResponse): FileTreeNode {
  const { children, is_dir: _isDirSnakeCase, isDir, kind, ...entry } = node;
  const directory = isDir ?? _isDirSnakeCase ?? kind === "directory";
  return {
    ...entry,
    kind: directory ? "directory" : "file",
    isDir: directory,
    children: children?.map(normalizeFileTreeNode),
  };
}

function parseSseRecord(record: string): SseEvent | null {
  const lines = record.split(/\r?\n/);
  let event = "message";
  let id: string | undefined;
  let retry: number | undefined;
  const data: string[] = [];

  for (const line of lines) {
    if (!line || line.startsWith(":")) continue;
    const separator = line.indexOf(":");
    const field = separator === -1 ? line : line.slice(0, separator);
    const value =
      separator === -1 ? "" : line.slice(separator + 1).replace(/^ /, "");
    if (field === "event") event = value;
    if (field === "id") id = value;
    if (field === "retry" && /^\d+$/.test(value)) retry = Number(value);
    if (field === "data") data.push(value);
  }

  if (data.length === 0) return null;
  return { event, data: data.join("\n"), id, retry };
}

export async function consumeSse(
  response: Response,
  handlers: SseHandlers,
  signal?: AbortSignal,
): Promise<void> {
  if (!response.ok) throw await responseError(response);
  if (!response.body)
    throw new ApiError(
      response.status,
      "The server returned an empty event stream",
    );

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  try {
    while (true) {
      if (signal?.aborted)
        throw new DOMException("Request aborted", "AbortError");
      const { done, value } = await reader.read();
      buffer += decoder.decode(value, { stream: !done });
      const records = buffer.split(/\r?\n\r?\n/);
      buffer = records.pop() ?? "";
      for (const record of records) {
        const event = parseSseRecord(record);
        if (event) handlers.onEvent(event);
      }
      if (done) break;
    }
    const finalEvent = parseSseRecord(buffer);
    if (finalEvent) handlers.onEvent(finalEvent);
  } catch (error) {
    if (isAbortError(error)) throw error;
    const normalized =
      error instanceof Error ? error : new Error(String(error));
    handlers.onError?.(normalized);
    throw normalized;
  } finally {
    reader.releaseLock();
  }
}

async function responseError(response: Response): Promise<ApiError> {
  const contentType = response.headers.get("content-type") ?? "";
  let body: ApiErrorResponse | undefined;
  let fallbackMessage =
    response.statusText || `Request failed (${response.status})`;
  try {
    if (contentType.includes("application/json")) {
      body = (await response.json()) as ApiErrorResponse;
    } else {
      const text = await response.text();
      if (text) fallbackMessage = text;
    }
  } catch {
    // Keep the status text when an intermediary returns malformed JSON.
  }
  return new ApiError(
    response.status,
    body?.error?.message ?? fallbackMessage,
    {
      code: body?.error?.code,
      requestId: body?.error?.requestId,
      details: body?.error?.details,
    },
  );
}

export function createApiClient(options: ApiClientOptions = {}) {
  const baseUrl = options.baseUrl ?? "/api/v2";
  const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
  let csrfToken: string | null = null;

  async function request<T>(
    path: string,
    init: RequestInit = {},
    options: RequestOptions = {},
  ): Promise<T> {
    const headers = new Headers(init.headers);
    for (const [key, value] of new Headers(options.headers))
      headers.set(key, value);
    headers.set("Accept", "application/json");
    const response = await fetchImpl(joinUrl(baseUrl, path), {
      ...init,
      headers,
      signal: options.signal,
      credentials: "include",
    });
    if (!response.ok) {
      const error = await responseError(response);
      if (error.status === 401) csrfToken = null;
      throw error;
    }
    if (response.status === 204) return undefined as T;
    return (await response.json()) as T;
  }

  async function getCsrf(signal?: AbortSignal): Promise<string> {
    if (csrfToken) return csrfToken;
    const result = await request<CsrfToken>(
      "/auth/csrf",
      { method: "POST" },
      { signal },
    );
    const token = result.csrfToken ?? result.token;
    if (!token)
      throw new ApiError(500, "The server did not return a CSRF token");
    csrfToken = token;
    return token;
  }

  async function write<T>(
    path: string,
    init: RequestInit,
    options: RequestOptions = {},
    needsCsrf = true,
  ): Promise<T> {
    const headers = new Headers(init.headers);
    if (needsCsrf) headers.set("X-CSRF-Token", await getCsrf(options.signal));
    return request<T>(path, { ...init, headers }, options);
  }

  function json<T>(
    method: string,
    path: string,
    body: unknown,
    options?: RequestOptions,
    needsCsrf = true,
  ): Promise<T> {
    return write<T>(
      path,
      {
        method,
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      },
      options,
      needsCsrf,
    );
  }

  function eventStream(
    path: string,
    init: RequestInit,
    handlers: SseHandlers,
    options: RequestOptions = {},
    needsCsrf = false,
  ): Promise<void> {
    return (async () => {
      const headers = new Headers(init.headers);
      for (const [key, value] of new Headers(options.headers))
        headers.set(key, value);
      headers.set("Accept", "text/event-stream");
      if (needsCsrf) headers.set("X-CSRF-Token", await getCsrf(options.signal));
      const response = await fetchImpl(joinUrl(baseUrl, path), {
        ...init,
        headers,
        signal: options.signal,
        credentials: "include",
      });
      if (!response.ok) {
        const error = await responseError(response);
        if (error.status === 401) csrfToken = null;
        throw error;
      }
      await consumeSse(response, handlers, options.signal);
    })();
  }

  return {
    auth: {
      session: async (options?: RequestOptions) => {
        const session = await request<Session>(
          "/auth/session",
          { method: "GET" },
          options,
        );
        csrfToken = session.csrfToken ?? null;
        return session;
      },
      login: async (input: { token: string }, options?: RequestOptions) => {
        const session = await json<Session>(
          "POST",
          "/auth/login",
          input,
          options,
          false,
        );
        csrfToken = session.csrfToken ?? null;
        return session;
      },
      logout: async (options?: RequestOptions) => {
        await write<void>("/auth/logout", { method: "POST" }, options);
        csrfToken = null;
      },
    },
    projects: {
      list: async (options?: RequestOptions) =>
        listFrom<Project>(
          await request<Project[] | ProjectListResponse>(
            "/projects",
            { method: "GET" },
            options,
          ),
        ),
      create: (input: CreateProjectInput, options?: RequestOptions) =>
        json<Project>("POST", "/projects", input, options),
      register: (input: RegisterProjectInput, options?: RequestOptions) =>
        json<Project>("POST", "/projects/register", input, options),
      get: (projectId: string, options?: RequestOptions) =>
        request<Project>(
          `/projects/${encodeURIComponent(projectId)}`,
          { method: "GET" },
          options,
        ),
      update: (
        projectId: string,
        input: UpdateProjectInput,
        options?: RequestOptions,
      ) =>
        json<Project>(
          "PATCH",
          `/projects/${encodeURIComponent(projectId)}`,
          input,
          options,
        ),
      remove: (projectId: string, options?: RequestOptions) =>
        write<void>(
          `/projects/${encodeURIComponent(projectId)}`,
          { method: "DELETE" },
          options,
        ),
    },
    files: {
      tree: async (projectId: string, options?: RequestOptions) => {
        const nodes = listFrom<FileTreeNodeResponse>(
          await request<FileTreeNodeResponse[] | FileTreeResponse>(
            `/projects/${encodeURIComponent(projectId)}/tree`,
            { method: "GET" },
            options,
          ),
        );
        return nodes.map(normalizeFileTreeNode);
      },
      content: (projectId: string, path: string, options?: RequestOptions) =>
        request<FileContent>(
          addQuery(`/projects/${encodeURIComponent(projectId)}/files/content`, {
            path,
          }),
          { method: "GET" },
          options,
        ),
      save: (
        projectId: string,
        path: string,
        input: SaveFileInput,
        options: RequestOptions = {},
      ) =>
        json<FileContent>(
          "PUT",
          addQuery(`/projects/${encodeURIComponent(projectId)}/files/content`, {
            path,
          }),
          input,
          {
            ...options,
            headers: {
              ...Object.fromEntries(new Headers(options.headers)),
              "If-Match": input.revision,
            },
          },
        ),
      createText: (
        projectId: string,
        path: string,
        content = "",
        options: RequestOptions = {},
      ) =>
        json<FileContent>(
          "PUT",
          addQuery(`/projects/${encodeURIComponent(projectId)}/files/content`, {
            path,
          }),
          { content, revision: "*" },
          {
            ...options,
            headers: {
              ...Object.fromEntries(new Headers(options.headers)),
              "If-Match": "*",
            },
          },
        ),
      createDirectory: (
        projectId: string,
        input: CreateDirectoryInput,
        options?: RequestOptions,
      ) =>
        json<CreateDirectoryResponse>(
          "POST",
          `/projects/${encodeURIComponent(projectId)}/directories`,
          input,
          options,
        ),
      move: (
        projectId: string,
        input: MoveFileInput,
        options?: RequestOptions,
      ) =>
        json<{ path?: string }>(
          "POST",
          `/projects/${encodeURIComponent(projectId)}/files/move`,
          input,
          options,
        ),
      remove: (
        projectId: string,
        path: string,
        revision?: string,
        options: RequestOptions = {},
      ) =>
        write<void>(
          addQuery(`/projects/${encodeURIComponent(projectId)}/files`, {
            path,
          }),
          {
            method: "DELETE",
            headers: revision
              ? { "Content-Type": "application/json", "If-Match": revision }
              : undefined,
            body: revision ? JSON.stringify({ revision }) : undefined,
          },
          options,
        ),
      upload: (
        projectId: string,
        files: File[],
        options: UploadOptions = {},
      ) => {
        const body = new FormData();
        for (const file of files) body.append("files", file);
        const { path = "raw/sources", ...requestOptions } = options;
        return write<UploadResponse>(
          addQuery(`/projects/${encodeURIComponent(projectId)}/uploads`, {
            path,
          }),
          { method: "POST", body },
          requestOptions,
        );
      },
      assetUrl: (projectId: string, path: string, download = false) =>
        joinUrl(
          baseUrl,
          addQuery(`/projects/${encodeURIComponent(projectId)}/assets`, {
            path,
            download: download || undefined,
          }),
        ),
    },
    search: {
      query: async (
        projectId: string,
        query: string,
        options?: RequestOptions,
      ) =>
        listFrom<SearchResult>(
          await json<SearchResult[] | SearchResponse>(
            "POST",
            `/projects/${encodeURIComponent(projectId)}/search`,
            { query },
            options,
          ),
        ),
      graph: (projectId: string, options?: RequestOptions) =>
        request<GraphData>(
          `/projects/${encodeURIComponent(projectId)}/graph`,
          { method: "GET" },
          options,
        ),
    },
    reviews: {
      list: async (projectId: string, options?: RequestOptions) =>
        listFrom<ReviewItem>(
          await request<ReviewItem[] | ReviewListResponse>(
            `/projects/${encodeURIComponent(projectId)}/reviews`,
            { method: "GET" },
            options,
          ),
        ),
      resolve: (
        projectId: string,
        reviewId: string,
        action: string,
        options?: RequestOptions,
      ) =>
        json<ReviewItem>(
          "PATCH",
          `/projects/${encodeURIComponent(projectId)}/reviews/${encodeURIComponent(reviewId)}`,
          { status: "resolved", action },
          options,
        ),
    },
    chat: {
      listSessions: async (projectId: string, options?: RequestOptions) =>
        listFrom<ChatSession>(
          await request<ChatSession[] | ChatSessionListResponse>(
            `/projects/${encodeURIComponent(projectId)}/chat/sessions`,
            { method: "GET" },
            options,
          ),
        ),
      createSession: (
        projectId: string,
        input: { title?: string } = {},
        options?: RequestOptions,
      ) =>
        json<ChatSession>(
          "POST",
          `/projects/${encodeURIComponent(projectId)}/chat/sessions`,
          input,
          options,
        ),
      session: (
        projectId: string,
        sessionId: string,
        options?: RequestOptions,
      ) =>
        request<ChatSessionDetail>(
          `/projects/${encodeURIComponent(projectId)}/chat/sessions/${encodeURIComponent(sessionId)}`,
          { method: "GET" },
          options,
        ),
      streamTurn: (
        projectId: string,
        sessionId: string,
        input: { message: string },
        handlers: SseHandlers,
        options?: RequestOptions,
      ) =>
        eventStream(
          `/projects/${encodeURIComponent(projectId)}/chat/sessions/${encodeURIComponent(sessionId)}/turns`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(input),
          },
          handlers,
          options,
          true,
        ),
      cancel: (
        projectId: string,
        sessionId: string,
        options?: RequestOptions,
      ) =>
        write<void>(
          `/projects/${encodeURIComponent(projectId)}/chat/sessions/${encodeURIComponent(sessionId)}/cancel`,
          { method: "POST" },
          options,
        ),
    },
    jobs: {
      list: async (projectId: string, options?: RequestOptions) =>
        listFrom<Job>(
          await request<Job[] | JobListResponse>(
            addQuery("/jobs", { projectId }),
            { method: "GET" },
            options,
          ),
        ),
      cancel: (jobId: string, options?: RequestOptions) =>
        write<void>(
          `/jobs/${encodeURIComponent(jobId)}/cancel`,
          { method: "POST" },
          options,
        ),
      retry: (jobId: string, options?: RequestOptions) =>
        write<Job>(
          `/jobs/${encodeURIComponent(jobId)}/retry`,
          { method: "POST" },
          options,
        ),
      streamProjectEvents: (
        projectId: string,
        handlers: SseHandlers,
        options?: RequestOptions,
      ) =>
        eventStream(
          addQuery("/events", { projectId }),
          { method: "GET" },
          handlers,
          options,
        ),
      streamJobEvents: (
        jobId: string,
        handlers: SseHandlers,
        options?: RequestOptions,
      ) =>
        eventStream(
          `/jobs/${encodeURIComponent(jobId)}/events`,
          { method: "GET" },
          handlers,
          options,
        ),
    },
    index: {
      rebuild: (projectId: string, options?: RequestOptions) =>
        write<Job>(
          `/projects/${encodeURIComponent(projectId)}/index/rebuild`,
          { method: "POST" },
          options,
        ),
    },
    settings: {
      get: (options?: RequestOptions) =>
        request<Settings>("/settings", { method: "GET" }, options),
      update: (patch: Settings, options?: RequestOptions) =>
        json<Settings>("PATCH", "/settings", patch, options),
      updateWebChat: (
        input: WebChatSettingsInput,
        options?: RequestOptions,
      ) => {
        const webChat: WebChatSettingsInput = {
          endpoint: input.endpoint,
          model: input.model,
        };
        if (input.apiKey?.trim()) webChat.apiKey = input.apiKey;
        return json<Settings>("PATCH", "/settings", { webChat }, options);
      },
    },
  };
}

export type ApiClient = ReturnType<typeof createApiClient>;
