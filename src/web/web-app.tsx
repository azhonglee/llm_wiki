import { useCallback, useEffect, useRef, useState } from "react";
import {
  Bot,
  ChevronDown,
  ChevronRight,
  Download,
  File,
  FileText,
  Folder,
  FolderPlus,
  GitFork,
  LoaderCircle,
  LogOut,
  MessageSquare,
  Network,
  Plus,
  RefreshCw,
  Save,
  Search,
  Send,
  Settings,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { ApiError, createApiClient, type ApiClient } from "@/api/client";
import type {
  ChatMessage,
  ChatSession,
  FileContent,
  FileTreeNode,
  GraphData,
  Job,
  Project,
  ReviewItem,
  Settings as SettingsData,
  SseEvent,
} from "@/api/contracts";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

const api = createApiClient();

type View =
  "files" | "search" | "graph" | "reviews" | "chat" | "jobs" | "settings";
type AuthState = "loading" | "anonymous" | "authenticated";

const VIEWS: Array<{ id: View; label: string; icon: typeof FileText }> = [
  { id: "files", label: "文件", icon: FileText },
  { id: "search", label: "搜索", icon: Search },
  { id: "graph", label: "图谱", icon: GitFork },
  { id: "reviews", label: "审阅", icon: FileText },
  { id: "chat", label: "对话", icon: MessageSquare },
  { id: "jobs", label: "任务", icon: LoaderCircle },
  { id: "settings", label: "设置", icon: Settings },
];

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    return error.code ? `${error.message} (${error.code})` : error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

function isDirectory(node: FileTreeNode): boolean {
  return node.isDir || node.kind === "directory";
}

function assetKind(
  path: string,
): "image" | "pdf" | "media" | "download" | null {
  const extension = path.split(".").pop()?.toLowerCase();
  if (extension === "svg") return "download";
  if (["png", "jpg", "jpeg", "gif", "webp"].includes(extension ?? ""))
    return "image";
  if (extension === "pdf") return "pdf";
  if (
    ["mp3", "wav", "ogg", "m4a", "mp4", "webm", "mov"].includes(extension ?? "")
  )
    return "media";
  if (
    [
      "doc",
      "docx",
      "ppt",
      "pptx",
      "xls",
      "xlsx",
      "odt",
      "ods",
      "odp",
      "epub",
      "mobi",
      "zip",
      "gz",
      "tar",
      "7z",
      "rar",
      "bin",
    ].includes(extension ?? "")
  )
    return "download";
  return null;
}

function eventText(event: SseEvent): string {
  if (event.event === "done") return "";
  try {
    const parsed: unknown = JSON.parse(event.data);
    if (typeof parsed === "string") return parsed === "[DONE]" ? "" : parsed;
    if (!parsed || typeof parsed !== "object") return "";
    const value = parsed as Record<string, unknown>;
    for (const key of ["delta", "content", "text", "message"]) {
      if (typeof value[key] === "string") return value[key];
    }
  } catch {
    // Some compatible SSE implementations stream plain text data.
  }
  return event.data === "[DONE]" ? "" : event.data;
}

function eventError(event: SseEvent): string {
  try {
    const parsed: unknown = JSON.parse(event.data);
    if (
      parsed &&
      typeof parsed === "object" &&
      typeof (parsed as Record<string, unknown>).error === "string"
    ) {
      return (parsed as Record<string, string>).error;
    }
  } catch {
    // Plain-text SSE error payloads are also supported.
  }
  return event.data;
}

function isSensitiveSettingKey(key: string): boolean {
  return /(?:api[-_]?key|token|secret|password|credential)/i.test(key);
}

function safeSettingsForEdit(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(safeSettingsForEdit);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .filter(([key]) => !isSensitiveSettingKey(key))
      .map(([key, entry]) => [key, safeSettingsForEdit(entry)]),
  );
}

function listValue(value: unknown): string {
  if (value === null || value === undefined) return "未配置";
  if (
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  )
    return String(value);
  if (Array.isArray(value)) return `${value.length} 项`;
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    if (typeof record.configured === "boolean") {
      return record.configured ? "已配置" : "未配置";
    }
    if (typeof record.source === "string") return record.source;
    return "已配置";
  }
  return "已配置";
}

function webChatValues(settings: SettingsData | null): {
  endpoint: string;
  model: string;
  apiKeyConfigured: boolean;
} {
  const webChat = settings?.webChat;
  if (!webChat || typeof webChat !== "object" || Array.isArray(webChat)) {
    return { endpoint: "", model: "", apiKeyConfigured: false };
  }
  const config = webChat as Record<string, unknown>;
  const apiKey = config.apiKey;
  return {
    endpoint: typeof config.endpoint === "string" ? config.endpoint : "",
    model: typeof config.model === "string" ? config.model : "",
    apiKeyConfigured: Boolean(
      apiKey &&
      typeof apiKey === "object" &&
      (apiKey as Record<string, unknown>).configured === true,
    ),
  };
}

function fileName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

export function WebApp({ client = api }: { client?: ApiClient }) {
  const [authState, setAuthState] = useState<AuthState>("loading");
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProject, setSelectedProject] = useState<Project | null>(null);
  const [activeView, setActiveView] = useState<View>("files");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const loadProjects = useCallback(
    async (signal?: AbortSignal) => {
      const loaded = await client.projects.list({ signal });
      setProjects(loaded);
      setSelectedProject((current) => current ?? loaded[0] ?? null);
    },
    [client],
  );

  useEffect(() => {
    const controller = new AbortController();
    void (async () => {
      try {
        const session = await client.auth.session({
          signal: controller.signal,
        });
        if (!session.authenticated) {
          setAuthState("anonymous");
          return;
        }
        setAuthState("authenticated");
        await loadProjects(controller.signal);
      } catch (error) {
        if (isAbortError(error)) return;
        if (error instanceof ApiError && error.status === 401) {
          setAuthState("anonymous");
          return;
        }
        setError(errorMessage(error));
        setAuthState("anonymous");
      }
    })();
    return () => controller.abort();
  }, [client, loadProjects]);

  const handleLogin = useCallback(
    async (input: { token: string }) => {
      setError(null);
      try {
        await client.auth.login(input);
        setAuthState("authenticated");
        await loadProjects();
      } catch (error) {
        setError(errorMessage(error));
        throw error;
      }
    },
    [client, loadProjects],
  );

  const handleLogout = useCallback(async () => {
    try {
      await client.auth.logout();
    } catch (error) {
      setError(errorMessage(error));
    } finally {
      setProjects([]);
      setSelectedProject(null);
      setAuthState("anonymous");
    }
  }, [client]);

  if (authState === "loading") {
    return <LoadingScreen label="正在连接 LLM Wiki..." />;
  }

  if (authState === "anonymous") {
    return <LoginScreen error={error} onLogin={handleLogin} />;
  }

  return (
    <div className="flex h-full min-w-0 flex-col bg-background text-foreground">
      <header className="flex h-12 shrink-0 items-center gap-3 border-b px-3">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <Bot className="size-5 shrink-0" />
          <span className="font-semibold">LLM Wiki</span>
          {selectedProject && (
            <span className="truncate text-sm text-muted-foreground">
              / {selectedProject.name}
            </span>
          )}
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={() =>
            void loadProjects().catch((error: unknown) =>
              setError(errorMessage(error)),
            )
          }
          title="刷新项目"
        >
          <RefreshCw />
          刷新
        </Button>
        <Button variant="ghost" size="sm" onClick={() => void handleLogout()}>
          <LogOut />
          退出登录
        </Button>
      </header>
      {error && (
        <StatusBar
          tone="error"
          message={error}
          onClose={() => setError(null)}
        />
      )}
      {notice && (
        <StatusBar
          tone="notice"
          message={notice}
          onClose={() => setNotice(null)}
        />
      )}
      <div className="flex min-h-0 flex-1">
        <ProjectSidebar
          projects={projects}
          selectedProject={selectedProject}
          onSelect={setSelectedProject}
          onCreate={async (input) => {
            const created = await client.projects.create(input);
            setProjects((items) => [
              created,
              ...items.filter((item) => item.id !== created.id),
            ]);
            setSelectedProject(created);
            setNotice("项目已创建");
          }}
          onRegister={async (path) => {
            const registered = await client.projects.register({
              relativePath: path,
            });
            setProjects((items) => [
              registered,
              ...items.filter((item) => item.id !== registered.id),
            ]);
            setSelectedProject(registered);
            setNotice("项目已注册");
          }}
          onRename={async (project, name) => {
            const updated = await client.projects.update(project.id, { name });
            setProjects((items) =>
              items.map((item) => (item.id === updated.id ? updated : item)),
            );
            setSelectedProject((current) =>
              current?.id === updated.id ? updated : current,
            );
            setNotice("项目已重命名");
          }}
          onUnregister={async (project) => {
            await client.projects.remove(project.id);
            const next = projects.filter((item) => item.id !== project.id);
            setProjects(next);
            setSelectedProject((current) =>
              current?.id === project.id ? (next[0] ?? null) : current,
            );
            setNotice("项目已解除注册");
          }}
          onError={(message) => setError(message)}
        />
        <nav className="flex w-12 shrink-0 flex-col items-center gap-1 border-r py-2">
          {VIEWS.map(({ id, label, icon: Icon }) => (
            <button
              key={id}
              type="button"
              onClick={() => setActiveView(id)}
              title={label}
              aria-label={label}
              className={cn(
                "flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
                activeView === id && "bg-muted text-foreground",
              )}
            >
              <Icon className="size-4" />
            </button>
          ))}
        </nav>
        <main className="min-w-0 flex-1 overflow-hidden">
          {!selectedProject ? (
            <EmptyProjectState />
          ) : (
            <ProjectView
              key={selectedProject.id}
              client={client}
              project={selectedProject}
              activeView={activeView}
              onError={(message) => setError(message)}
              onNotice={setNotice}
            />
          )}
        </main>
      </div>
    </div>
  );
}

function LoadingScreen({ label }: { label: string }) {
  return (
    <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
      <LoaderCircle className="size-4 animate-spin" />
      {label}
    </div>
  );
}

function StatusBar({
  tone,
  message,
  onClose,
}: {
  tone: "error" | "notice";
  message: string;
  onClose: () => void;
}) {
  return (
    <div
      className={cn(
        "flex items-center gap-2 border-b px-3 py-2 text-sm",
        tone === "error"
          ? "border-destructive/30 bg-destructive/10 text-destructive"
          : "bg-muted text-muted-foreground",
      )}
    >
      <span className="min-w-0 flex-1 break-words">{message}</span>
      <button
        type="button"
        className="rounded p-0.5 hover:bg-background/50"
        onClick={onClose}
        aria-label="关闭"
      >
        <X className="size-4" />
      </button>
    </div>
  );
}

function LoginScreen({
  error,
  onLogin,
}: {
  error: string | null;
  onLogin: (input: { token: string }) => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!value.trim()) {
      setLocalError("请输入初始化令牌。");
      return;
    }
    setSubmitting(true);
    setLocalError(null);
    try {
      await onLogin({ token: value.trim() });
    } catch {
      // The parent exposes the normalized API error and keeps it visible.
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="flex h-full items-center justify-center bg-muted/20 px-4">
      <form
        className="w-full max-w-sm rounded-xl border bg-card p-6 shadow-sm"
        onSubmit={(event) => void submit(event)}
      >
        <div className="mb-6 flex items-center gap-2">
          <Bot className="size-6" />
          <div>
            <h1 className="font-semibold">LLM Wiki</h1>
            <p className="text-sm text-muted-foreground">登录本地服务</p>
          </div>
        </div>
        <label
          className="mb-2 block text-sm font-medium"
          htmlFor="web-login-value"
        >
          初始化令牌
        </label>
        <Input
          id="web-login-value"
          type="password"
          value={value}
          onChange={(event) => setValue(event.target.value)}
          autoComplete="off"
        />
        {(localError ?? error) && (
          <p className="mt-3 text-sm text-destructive">{localError ?? error}</p>
        )}
        <Button className="mt-5 w-full" type="submit" disabled={submitting}>
          {submitting ? <LoaderCircle className="animate-spin" /> : null}
          登录
        </Button>
      </form>
    </div>
  );
}

function ProjectSidebar({
  projects,
  selectedProject,
  onSelect,
  onCreate,
  onRegister,
  onRename,
  onUnregister,
  onError,
}: {
  projects: Project[];
  selectedProject: Project | null;
  onSelect: (project: Project) => void;
  onCreate: (input: { name: string }) => Promise<void>;
  onRegister: (path: string) => Promise<void>;
  onRename: (project: Project, name: string) => Promise<void>;
  onUnregister: (project: Project) => Promise<void>;
  onError: (message: string) => void;
}) {
  const [newName, setNewName] = useState("");
  const [registerPath, setRegisterPath] = useState("");
  const [rename, setRename] = useState("");
  const [creating, setCreating] = useState(false);
  const [registering, setRegistering] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [unregistering, setUnregistering] = useState(false);

  useEffect(() => setRename(selectedProject?.name ?? ""), [selectedProject]);

  async function create() {
    if (!newName.trim()) return;
    setCreating(true);
    try {
      await onCreate({ name: newName.trim() });
      setNewName("");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setCreating(false);
    }
  }

  async function register() {
    if (!registerPath.trim()) return;
    setRegistering(true);
    try {
      await onRegister(registerPath.trim());
      setRegisterPath("");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setRegistering(false);
    }
  }

  async function renameProject() {
    if (
      !selectedProject ||
      !rename.trim() ||
      rename.trim() === selectedProject.name
    )
      return;
    setRenaming(true);
    try {
      await onRename(selectedProject, rename.trim());
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setRenaming(false);
    }
  }

  async function unregisterProject() {
    if (
      !selectedProject ||
      !window.confirm(
        `确认解除注册项目“${selectedProject.name}”？项目磁盘内容不会删除。`,
      )
    )
      return;
    setUnregistering(true);
    try {
      await onUnregister(selectedProject);
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setUnregistering(false);
    }
  }

  return (
    <aside className="flex w-64 shrink-0 flex-col border-r bg-muted/15">
      <div className="border-b p-3">
        <div className="mb-2 text-xs font-medium tracking-wide text-muted-foreground">
          项目
        </div>
        <div className="flex gap-1">
          <Input
            value={newName}
            onChange={(event) => setNewName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void create();
            }}
            placeholder="新项目名称"
            aria-label="新项目名称"
          />
          <Button
            size="icon"
            onClick={() => void create()}
            disabled={creating || !newName.trim()}
            title="创建项目"
          >
            <Plus />
          </Button>
        </div>
        <div className="mt-2 flex gap-1">
          <Input
            value={registerPath}
            onChange={(event) => setRegisterPath(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void register();
            }}
            placeholder="已有项目相对路径"
            aria-label="已有项目相对路径"
          />
          <Button
            variant="outline"
            size="icon"
            onClick={() => void register()}
            disabled={registering || !registerPath.trim()}
            title="注册项目"
          >
            <FolderPlus />
          </Button>
        </div>
        <p className="mt-1 text-[11px] text-muted-foreground">
          只能注册服务端工作区内的相对路径。
        </p>
      </div>
      {selectedProject && (
        <div className="border-b p-3">
          <div className="mb-2 text-xs font-medium tracking-wide text-muted-foreground">
            当前项目
          </div>
          <div className="flex gap-1">
            <Input
              value={rename}
              onChange={(event) => setRename(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void renameProject();
              }}
              aria-label="项目名称"
            />
            <Button
              variant="outline"
              size="icon"
              onClick={() => void renameProject()}
              disabled={
                renaming ||
                !rename.trim() ||
                rename.trim() === selectedProject.name
              }
              title="重命名项目"
            >
              <Save />
            </Button>
          </div>
          <Button
            className="mt-2 w-full"
            variant="destructive"
            size="sm"
            onClick={() => void unregisterProject()}
            disabled={unregistering}
          >
            <Trash2 /> 解除注册
          </Button>
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {projects.length === 0 ? (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            还没有项目。
          </p>
        ) : (
          projects.map((project) => (
            <button
              key={project.id}
              type="button"
              onClick={() => onSelect(project)}
              className={cn(
                "mb-1 flex w-full flex-col rounded-md px-2 py-2 text-left hover:bg-muted",
                selectedProject?.id === project.id && "bg-muted",
              )}
            >
              <span className="truncate text-sm font-medium">
                {project.name}
              </span>
              <span className="truncate text-xs text-muted-foreground">
                {project.id}
              </span>
            </button>
          ))
        )}
      </div>
    </aside>
  );
}

function EmptyProjectState() {
  return (
    <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
      创建或注册项目后即可开始。
    </div>
  );
}

function ProjectView({
  client,
  project,
  activeView,
  onError,
  onNotice,
}: {
  client: ApiClient;
  project: Project;
  activeView: View;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
}) {
  switch (activeView) {
    case "files":
      return (
        <FilesView
          client={client}
          project={project}
          onError={onError}
          onNotice={onNotice}
        />
      );
    case "search":
      return <SearchView client={client} project={project} onError={onError} />;
    case "graph":
      return <GraphView client={client} project={project} onError={onError} />;
    case "reviews":
      return (
        <ReviewsView client={client} project={project} onError={onError} />
      );
    case "chat":
      return <ChatView client={client} project={project} onError={onError} />;
    case "jobs":
      return (
        <JobsView
          client={client}
          project={project}
          onError={onError}
          onNotice={onNotice}
        />
      );
    case "settings":
      return (
        <SettingsView client={client} onError={onError} onNotice={onNotice} />
      );
  }
}

function FilesView({
  client,
  project,
  onError,
  onNotice,
}: {
  client: ApiClient;
  project: Project;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
}) {
  const [tree, setTree] = useState<FileTreeNode[]>([]);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [content, setContent] = useState<FileContent | null>(null);
  const [draft, setDraft] = useState("");
  const [loadingTree, setLoadingTree] = useState(true);
  const [loadingFile, setLoadingFile] = useState(false);
  const [saving, setSaving] = useState(false);
  const [directoryPath, setDirectoryPath] = useState("");
  const [markdownPath, setMarkdownPath] = useState("wiki/untitled.md");
  const [movePath, setMovePath] = useState("");
  const [uploadPath, setUploadPath] = useState("raw/sources");
  const [creatingFile, setCreatingFile] = useState(false);
  const [moving, setMoving] = useState(false);
  const [uploading, setUploading] = useState(false);
  const fileController = useRef<AbortController | null>(null);

  const refreshTree = useCallback(
    async (signal?: AbortSignal) => {
      setLoadingTree(true);
      try {
        setTree(await client.files.tree(project.id, { signal }));
      } catch (error) {
        if (!isAbortError(error)) onError(errorMessage(error));
      } finally {
        if (!signal?.aborted) setLoadingTree(false);
      }
    },
    [client, onError, project.id],
  );

  useEffect(() => {
    const controller = new AbortController();
    void refreshTree(controller.signal);
    return () => controller.abort();
  }, [refreshTree]);

  async function open(path: string) {
    const kind = assetKind(path);
    setSelectedPath(path);
    setMovePath(path);
    setContent(null);
    setDraft("");
    if (kind) return;
    fileController.current?.abort();
    const controller = new AbortController();
    fileController.current = controller;
    setLoadingFile(true);
    try {
      const loaded = await client.files.content(project.id, path, {
        signal: controller.signal,
      });
      if (!controller.signal.aborted) {
        setContent(loaded);
        setDraft(loaded.content);
      }
    } catch (error) {
      if (!isAbortError(error)) onError(errorMessage(error));
    } finally {
      if (!controller.signal.aborted) setLoadingFile(false);
    }
  }

  async function save() {
    if (!content || !selectedPath) return;
    setSaving(true);
    try {
      const saved = await client.files.save(project.id, selectedPath, {
        content: draft,
        revision: content.revision,
      });
      const next = { ...saved, content: saved.content ?? draft };
      setContent(next);
      setDraft(next.content);
      onNotice("文件已保存");
      await refreshTree();
    } catch (error) {
      if (error instanceof ApiError && error.status === 409)
        onError("文件已在服务器端变更，请重新加载后再保存。");
      else onError(errorMessage(error));
    } finally {
      setSaving(false);
    }
  }

  async function createDirectory() {
    if (!directoryPath.trim()) return;
    try {
      await client.files.createDirectory(project.id, {
        path: directoryPath.trim(),
      });
      setDirectoryPath("");
      await refreshTree();
      onNotice("目录已创建");
    } catch (error) {
      onError(errorMessage(error));
    }
  }

  async function createMarkdown() {
    if (!markdownPath.trim()) return;
    setCreatingFile(true);
    try {
      const created = await client.files.createText(
        project.id,
        markdownPath.trim(),
      );
      await refreshTree();
      await open(created.path || markdownPath.trim());
      onNotice("Markdown 文件已创建");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setCreatingFile(false);
    }
  }

  async function move() {
    if (!selectedPath || !movePath.trim() || movePath.trim() === selectedPath)
      return;
    setMoving(true);
    try {
      await client.files.move(project.id, {
        sourcePath: selectedPath,
        targetPath: movePath.trim(),
      });
      await refreshTree();
      await open(movePath.trim());
      onNotice("文件已移动");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setMoving(false);
    }
  }

  async function remove() {
    if (!selectedPath || !window.confirm(`确认删除“${selectedPath}”？`)) return;
    try {
      await client.files.remove(project.id, selectedPath, content?.revision);
      setSelectedPath(null);
      setMovePath("");
      setContent(null);
      setDraft("");
      await refreshTree();
      onNotice("文件已删除");
    } catch (error) {
      onError(errorMessage(error));
    }
  }

  async function upload(files: FileList | null) {
    if (!files?.length) return;
    setUploading(true);
    try {
      await client.files.upload(project.id, Array.from(files), {
        path: uploadPath.trim() || "raw/sources",
      });
      await refreshTree();
      onNotice(`已上传 ${files.length} 个文件`);
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setUploading(false);
    }
  }

  const kind = selectedPath ? assetKind(selectedPath) : null;
  return (
    <div className="flex h-full min-w-0">
      <aside className="flex w-72 shrink-0 flex-col border-r">
        <div className="space-y-2 border-b p-2">
          <div className="flex items-center justify-between">
            <span className="text-sm font-medium">文件</span>
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={() => void refreshTree()}
              title="刷新"
            >
              <RefreshCw />
            </Button>
          </div>
          <div className="flex gap-1">
            <Input
              value={directoryPath}
              onChange={(event) => setDirectoryPath(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void createDirectory();
              }}
              placeholder="新目录路径"
              aria-label="新目录路径"
            />
            <Button
              variant="outline"
              size="icon"
              onClick={() => void createDirectory()}
              disabled={!directoryPath.trim()}
              title="创建目录"
            >
              <FolderPlus />
            </Button>
          </div>
          <div className="flex gap-1">
            <Input
              value={markdownPath}
              onChange={(event) => setMarkdownPath(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void createMarkdown();
              }}
              placeholder="新 Markdown 路径"
              aria-label="新 Markdown 路径"
            />
            <Button
              variant="outline"
              size="icon"
              onClick={() => void createMarkdown()}
              disabled={creatingFile || !markdownPath.trim()}
              title="新建 Markdown 文件"
            >
              <FileText />
            </Button>
          </div>
          <Input
            value={uploadPath}
            onChange={(event) => setUploadPath(event.target.value)}
            placeholder="上传目标目录"
            aria-label="上传目标目录"
          />
          <label className="flex cursor-pointer items-center justify-center gap-1 rounded-md border border-dashed px-2 py-1.5 text-xs text-muted-foreground hover:bg-muted">
            {uploading ? (
              <LoaderCircle className="size-3.5 animate-spin" />
            ) : (
              <Upload className="size-3.5" />
            )}{" "}
            上传文件
            <input
              className="sr-only"
              type="file"
              multiple
              disabled={uploading}
              onChange={(event) => {
                void upload(event.target.files);
                event.currentTarget.value = "";
              }}
            />
          </label>
          <p className="text-[11px] text-muted-foreground">
            默认上传到 raw/sources，可修改目标目录。
          </p>
          {selectedPath && (
            <div className="flex gap-1 border-t pt-2">
              <Input
                value={movePath}
                onChange={(event) => setMovePath(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") void move();
                }}
                aria-label="移动或重命名后的路径"
              />
              <Button
                variant="outline"
                size="icon"
                onClick={() => void move()}
                disabled={
                  moving || !movePath.trim() || movePath.trim() === selectedPath
                }
                title="移动或重命名"
              >
                <GitFork />
              </Button>
            </div>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-auto p-2">
          {loadingTree ? (
            <LoadingScreen label="正在加载文件..." />
          ) : (
            <FileTree
              nodes={tree}
              selectedPath={selectedPath}
              onOpen={(path) => void open(path)}
            />
          )}
        </div>
      </aside>
      <section className="flex min-w-0 flex-1 flex-col">
        {!selectedPath ? (
          <EmptyPanel icon={FileText} label="选择文件以查看或编辑。" />
        ) : kind ? (
          <AssetPreview
            client={client}
            projectId={project.id}
            path={selectedPath}
            kind={kind}
          />
        ) : loadingFile ? (
          <LoadingScreen label="正在加载文件..." />
        ) : content ? (
          <>
            <div className="flex shrink-0 items-center gap-2 border-b px-3 py-2">
              <span className="min-w-0 flex-1 truncate text-sm font-medium">
                {content.path}
              </span>
              <Button
                variant="destructive"
                size="sm"
                onClick={() => void remove()}
              >
                <Trash2 /> 删除
              </Button>
              <Button
                size="sm"
                onClick={() => void save()}
                disabled={saving || draft === content.content}
              >
                {saving ? <LoaderCircle className="animate-spin" /> : <Save />}{" "}
                保存
              </Button>
            </div>
            <textarea
              className="min-h-0 flex-1 resize-none bg-background p-4 font-mono text-sm leading-6 outline-none"
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              spellCheck={false}
              aria-label="文件编辑器"
            />
          </>
        ) : (
          <EmptyPanel icon={FileText} label="此文件无法按文本显示。" />
        )}
      </section>
    </div>
  );
}

function FileTree({
  nodes,
  selectedPath,
  onOpen,
  depth = 0,
}: {
  nodes: FileTreeNode[];
  selectedPath: string | null;
  onOpen: (path: string) => void;
  depth?: number;
}) {
  return (
    <>
      {nodes.map((node) => (
        <FileTreeItem
          key={node.path}
          node={node}
          selectedPath={selectedPath}
          onOpen={onOpen}
          depth={depth}
        />
      ))}
    </>
  );
}

function FileTreeItem({
  node,
  selectedPath,
  onOpen,
  depth,
}: {
  node: FileTreeNode;
  selectedPath: string | null;
  onOpen: (path: string) => void;
  depth: number;
}) {
  const directory = isDirectory(node);
  const [expanded, setExpanded] = useState(true);
  return (
    <div>
      <button
        type="button"
        onClick={() =>
          directory ? setExpanded((value) => !value) : onOpen(node.path)
        }
        className={cn(
          "flex w-full items-center gap-1 rounded px-1.5 py-1 text-left text-sm hover:bg-muted",
          selectedPath === node.path && "bg-muted",
        )}
        style={{ paddingLeft: `${depth * 12 + 6}px` }}
      >
        {directory ? (
          expanded ? (
            <ChevronDown className="size-3.5 shrink-0" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0" />
          )
        ) : (
          <span className="w-3.5" />
        )}
        {directory ? (
          <Folder className="size-3.5 shrink-0 text-muted-foreground" />
        ) : (
          <File className="size-3.5 shrink-0 text-muted-foreground" />
        )}
        <span className="truncate">{node.name}</span>
      </button>
      {directory && expanded && node.children && (
        <FileTree
          nodes={node.children}
          selectedPath={selectedPath}
          onOpen={onOpen}
          depth={depth + 1}
        />
      )}
    </div>
  );
}

function AssetPreview({
  client,
  projectId,
  path,
  kind,
}: {
  client: ApiClient;
  projectId: string;
  path: string;
  kind: "image" | "pdf" | "media" | "download";
}) {
  const source = client.files.assetUrl(projectId, path);
  const download = client.files.assetUrl(projectId, path, true);
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b px-3 py-2">
        <span className="min-w-0 flex-1 truncate text-sm font-medium">
          {path}
        </span>
        <a
          className="inline-flex h-7 items-center gap-1 rounded-md border px-2 text-xs hover:bg-muted"
          href={download}
        >
          <Download className="size-3.5" /> 下载
        </a>
      </div>
      <div className="min-h-0 flex-1 bg-muted/20 p-3">
        {kind === "image" && (
          <img
            className="h-full w-full object-contain"
            src={source}
            alt={fileName(path)}
          />
        )}
        {kind === "pdf" && (
          <iframe
            className="size-full rounded border bg-background"
            src={source}
            title={fileName(path)}
          />
        )}
        {kind === "media" && <MediaPreview path={path} source={source} />}
        {kind === "download" && (
          <div className="flex h-full flex-col items-center justify-center gap-3 text-center text-sm text-muted-foreground">
            <File className="size-8 opacity-40" />
            <p>此文件类型不会内联预览，请下载后查看。</p>
          </div>
        )}
      </div>
    </div>
  );
}

function MediaPreview({ path, source }: { path: string; source: string }) {
  const extension = path.split(".").pop()?.toLowerCase();
  if (["mp4", "webm", "mov"].includes(extension ?? ""))
    return <video className="size-full" controls src={source} />;
  return <audio className="w-full" controls src={source} />;
}

function SearchView({
  client,
  project,
  onError,
}: {
  client: ApiClient;
  project: Project;
  onError: (message: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<
    Array<{ path: string; title: string; snippet?: string; score?: number }>
  >([]);
  const [loading, setLoading] = useState(false);

  async function search() {
    if (!query.trim()) return;
    setLoading(true);
    try {
      setResults(await client.search.query(project.id, query.trim()));
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 gap-2 border-b p-3">
        <Input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") void search();
          }}
          placeholder="搜索当前项目"
          aria-label="搜索当前项目"
        />
        <Button
          onClick={() => void search()}
          disabled={loading || !query.trim()}
        >
          {loading ? <LoaderCircle className="animate-spin" /> : <Search />}{" "}
          Search
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-4">
        {results.length === 0 ? (
          <EmptyPanel
            icon={Search}
            label={loading ? "正在搜索..." : "搜索结果将显示在这里。"}
          />
        ) : (
          results.map((result) => (
            <article key={result.path} className="mb-3 rounded-lg border p-3">
              <div className="flex gap-3">
                <FileText className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0">
                  <h2 className="truncate text-sm font-medium">
                    {result.title || fileName(result.path)}
                  </h2>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {result.path}
                  </p>
                  {result.snippet && (
                    <p className="mt-2 whitespace-pre-wrap text-sm text-muted-foreground">
                      {result.snippet}
                    </p>
                  )}
                </div>
              </div>
            </article>
          ))
        )}
      </div>
    </div>
  );
}

function GraphView({
  client,
  project,
  onError,
}: {
  client: ApiClient;
  project: Project;
  onError: (message: string) => void;
}) {
  const [graph, setGraph] = useState<GraphData | null>(null);
  const [loading, setLoading] = useState(true);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      try {
        setGraph(await client.search.graph(project.id, { signal }));
      } catch (error) {
        if (!isAbortError(error)) onError(errorMessage(error));
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [client, onError, project.id],
  );
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);
  if (loading) return <LoadingScreen label="正在加载图谱..." />;
  if (!graph) return <EmptyPanel icon={GitFork} label="图谱不可用。" />;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-3 border-b px-4 py-3">
        <Network className="size-4" />
        <span className="font-medium">知识图谱</span>
        <span className="text-sm text-muted-foreground">
          {graph.nodes.length} 个节点，{graph.edges.length} 条连接
        </span>
        <Button
          className="ml-auto"
          variant="ghost"
          size="sm"
          onClick={() => void load()}
        >
          <RefreshCw /> 刷新
        </Button>
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-1 gap-0 overflow-auto md:grid-cols-2">
        <section className="border-b p-4 md:border-b-0 md:border-r">
          <h2 className="mb-3 text-sm font-medium">节点</h2>
          {graph.nodes.map((node) => (
            <div key={node.id} className="mb-2 rounded-md border px-3 py-2">
              <div className="text-sm font-medium">{node.label ?? node.id}</div>
              <div className="text-xs text-muted-foreground">
                {node.path ?? node.type ?? "页面"}
              </div>
            </div>
          ))}
        </section>
        <section className="p-4">
          <h2 className="mb-3 text-sm font-medium">连接</h2>
          {graph.edges.map((edge, index) => (
            <div
              key={edge.id ?? `${edge.source}-${edge.target}-${index}`}
              className="mb-2 rounded-md border px-3 py-2 text-sm"
            >
              <span>{edge.source}</span>
              <span className="mx-2 text-muted-foreground">-&gt;</span>
              <span>{edge.target}</span>
            </div>
          ))}
        </section>
      </div>
    </div>
  );
}

function ReviewsView({
  client,
  project,
  onError,
}: {
  client: ApiClient;
  project: Project;
  onError: (message: string) => void;
}) {
  const [reviews, setReviews] = useState<ReviewItem[]>([]);
  const [loading, setLoading] = useState(true);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      try {
        setReviews(await client.reviews.list(project.id, { signal }));
      } catch (error) {
        if (!isAbortError(error)) onError(errorMessage(error));
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [client, onError, project.id],
  );
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);
  async function resolve(review: ReviewItem, action: string) {
    try {
      const updated = await client.reviews.resolve(
        project.id,
        review.id,
        action,
      );
      setReviews((items) =>
        items.map((item) => (item.id === review.id ? updated : item)),
      );
    } catch (error) {
      onError(errorMessage(error));
    }
  }
  if (loading) return <LoadingScreen label="正在加载审阅项..." />;
  return (
    <div className="h-full overflow-auto p-4">
      <div className="mb-4 flex items-center">
        <h1 className="font-medium">审阅队列</h1>
        <Button
          className="ml-auto"
          variant="ghost"
          size="sm"
          onClick={() => void load()}
        >
          <RefreshCw /> 刷新
        </Button>
      </div>
      {reviews.length === 0 ? (
        <EmptyPanel icon={FileText} label="没有审阅项。" />
      ) : (
        reviews.map((review) => (
          <article key={review.id} className="mb-3 rounded-lg border p-4">
            <div className="flex items-start gap-3">
              <FileText className="mt-0.5 size-4 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <h2 className="font-medium">{review.title}</h2>
                  <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                    {review.type ?? review.status ?? "审阅"}
                  </span>
                </div>
                {review.description && (
                  <p className="mt-2 whitespace-pre-wrap text-sm text-muted-foreground">
                    {review.description}
                  </p>
                )}
                <div className="mt-3 flex flex-wrap gap-2">
                  {(review.options?.length
                    ? review.options
                    : [{ action: "resolve", label: "处理" }]
                  ).map((option) => (
                    <Button
                      key={option.action}
                      size="sm"
                      variant="outline"
                      onClick={() => void resolve(review, option.action)}
                      disabled={review.resolved || review.status === "resolved"}
                    >
                      {option.label}
                    </Button>
                  ))}
                </div>
              </div>
            </div>
          </article>
        ))
      )}
    </div>
  );
}

function ChatView({
  client,
  project,
  onError,
}: {
  client: ApiClient;
  project: Project;
  onError: (message: string) => void;
}) {
  const [sessions, setSessions] = useState<ChatSession[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(
    null,
  );
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState(false);
  const streamController = useRef<AbortController | null>(null);
  const draftRef = useRef("");

  const loadSessions = useCallback(
    async (signal?: AbortSignal) => {
      try {
        const loaded = await client.chat.listSessions(project.id, { signal });
        setSessions(loaded);
        setSelectedSessionId((current) => current ?? loaded[0]?.id ?? null);
      } catch (error) {
        if (!isAbortError(error)) onError(errorMessage(error));
      }
    },
    [client, onError, project.id],
  );
  useEffect(() => {
    const controller = new AbortController();
    void loadSessions(controller.signal);
    return () => controller.abort();
  }, [loadSessions]);
  useEffect(() => {
    if (!selectedSessionId) {
      setMessages([]);
      return;
    }
    const controller = new AbortController();
    void client.chat
      .session(project.id, selectedSessionId, { signal: controller.signal })
      .then((session) => setMessages(session.messages ?? []))
      .catch((error: unknown) => {
        if (!isAbortError(error)) onError(errorMessage(error));
      });
    return () => controller.abort();
  }, [client, onError, project.id, selectedSessionId]);

  async function createSession() {
    try {
      const session = await client.chat.createSession(project.id);
      setSessions((items) => [session, ...items]);
      setSelectedSessionId(session.id);
      setMessages([]);
    } catch (error) {
      onError(errorMessage(error));
    }
  }
  async function send() {
    if (!input.trim() || streaming) return;
    let sessionId = selectedSessionId;
    if (!sessionId) {
      try {
        const session = await client.chat.createSession(project.id);
        setSessions((items) => [session, ...items]);
        setSelectedSessionId(session.id);
        sessionId = session.id;
      } catch (error) {
        onError(errorMessage(error));
        return;
      }
    }
    const message = input.trim();
    setInput("");
    setMessages((items) => [...items, { role: "user", content: message }]);
    draftRef.current = "";
    setDraft("");
    setStreaming(true);
    const controller = new AbortController();
    streamController.current = controller;
    try {
      await client.chat.streamTurn(
        project.id,
        sessionId,
        { message },
        {
          onEvent: (event) => {
            if (event.event === "error") {
              onError(eventError(event));
              return;
            }
            if (event.event !== "delta" && event.event !== "message") return;
            const text = eventText(event);
            if (!text) return;
            draftRef.current += text;
            setDraft(draftRef.current);
          },
        },
        { signal: controller.signal },
      );
      if (draftRef.current) {
        setMessages((items) => [
          ...items,
          { role: "assistant", content: draftRef.current },
        ]);
      }
      draftRef.current = "";
      setDraft("");
      await loadSessions();
    } catch (error) {
      if (!isAbortError(error)) onError(errorMessage(error));
    } finally {
      if (!controller.signal.aborted) setStreaming(false);
    }
  }
  async function cancel() {
    streamController.current?.abort();
    if (selectedSessionId) {
      try {
        await client.chat.cancel(project.id, selectedSessionId);
      } catch (error) {
        if (!isAbortError(error)) onError(errorMessage(error));
      }
    }
    setStreaming(false);
  }
  return (
    <div className="flex h-full min-w-0">
      <aside className="flex w-56 shrink-0 flex-col border-r">
        <div className="border-b p-2">
          <Button
            className="w-full"
            size="sm"
            onClick={() => void createSession()}
          >
            <Plus /> 新建对话
          </Button>
        </div>
        <div className="min-h-0 flex-1 overflow-auto p-2">
          {sessions.map((session) => (
            <button
              key={session.id}
              type="button"
              className={cn(
                "mb-1 w-full rounded px-2 py-2 text-left text-sm hover:bg-muted",
                selectedSessionId === session.id && "bg-muted",
              )}
              onClick={() => setSelectedSessionId(session.id)}
            >
              {session.title ?? "未命名对话"}
            </button>
          ))}
        </div>
      </aside>
      <section className="flex min-w-0 flex-1 flex-col">
        <div className="min-h-0 flex-1 overflow-auto p-4">
          {messages.length === 0 && !draft ? (
            <EmptyPanel icon={MessageSquare} label="开始围绕此项目进行对话。" />
          ) : (
            <>
              {messages.map((message, index) => (
                <ChatBubble
                  key={`${message.id ?? message.role}-${index}`}
                  message={message}
                />
              ))}
              {draft && (
                <ChatBubble
                  message={{ role: "assistant", content: draft }}
                  streaming
                />
              )}
            </>
          )}
        </div>
        <div className="border-t p-3">
          <div className="flex gap-2">
            <textarea
              className="min-h-16 flex-1 resize-none rounded-lg border bg-background px-3 py-2 text-sm outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
              value={input}
              onChange={(event) => setInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  void send();
                }
              }}
              placeholder="询问此项目的内容"
              aria-label="对话消息"
            />
            {streaming ? (
              <Button variant="outline" onClick={() => void cancel()}>
                <X /> 停止
              </Button>
            ) : (
              <Button onClick={() => void send()} disabled={!input.trim()}>
                <Send /> 发送
              </Button>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}

function ChatBubble({
  message,
  streaming = false,
}: {
  message: ChatMessage;
  streaming?: boolean;
}) {
  return (
    <div
      className={cn(
        "mb-3 max-w-3xl rounded-lg px-3 py-2 text-sm whitespace-pre-wrap",
        message.role === "user"
          ? "ml-auto bg-primary text-primary-foreground"
          : "bg-muted",
      )}
    >
      <div className="mb-1 text-[11px] font-medium opacity-70">
        {message.role === "user" ? "你" : "助手"}
        {streaming ? " 正在输入..." : ""}
      </div>
      {message.content}
    </div>
  );
}

function JobsView({
  client,
  project,
  onError,
  onNotice,
}: {
  client: ApiClient;
  project: Project;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
}) {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [events, setEvents] = useState<SseEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [rebuilding, setRebuilding] = useState(false);
  const [retryingId, setRetryingId] = useState<string | null>(null);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      try {
        setJobs(await client.jobs.list(project.id, { signal }));
      } catch (error) {
        if (!isAbortError(error)) onError(errorMessage(error));
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [client, onError, project.id],
  );
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    void client.jobs
      .streamProjectEvents(
        project.id,
        {
          onEvent: (event) => {
            setEvents((items) => [event, ...items].slice(0, 20));
            void load();
          },
          onError: (error) => {
            if (!isAbortError(error)) onError(errorMessage(error));
          },
        },
        { signal: controller.signal },
      )
      .catch((error: unknown) => {
        if (!isAbortError(error)) onError(errorMessage(error));
      });
    return () => controller.abort();
  }, [client, load, onError, project.id]);
  async function cancel(job: Job) {
    try {
      await client.jobs.cancel(job.id);
      await load();
    } catch (error) {
      onError(errorMessage(error));
    }
  }
  async function rebuild() {
    setRebuilding(true);
    try {
      await client.index.rebuild(project.id);
      await load();
      onNotice("已创建重建索引任务");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setRebuilding(false);
    }
  }
  async function retry(job: Job) {
    setRetryingId(job.id);
    try {
      await client.jobs.retry(job.id);
      await load();
      onNotice("已重试失败任务");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setRetryingId(null);
    }
  }
  if (loading) return <LoadingScreen label="正在加载任务..." />;
  return (
    <div className="grid h-full min-h-0 grid-cols-1 md:grid-cols-[minmax(0,1fr)_20rem]">
      <section className="min-h-0 overflow-auto p-4">
        <div className="mb-4 flex items-center gap-2">
          <h1 className="font-medium">任务</h1>
          <Button
            className="ml-auto"
            size="sm"
            onClick={() => void rebuild()}
            disabled={rebuilding}
          >
            {rebuilding ? (
              <LoaderCircle className="animate-spin" />
            ) : (
              <RefreshCw />
            )}{" "}
            重建索引
          </Button>
          <Button variant="ghost" size="sm" onClick={() => void load()}>
            <RefreshCw /> 刷新
          </Button>
        </div>
        {jobs.length === 0 ? (
          <EmptyPanel icon={LoaderCircle} label="该项目暂无任务。" />
        ) : (
          jobs.map((job) => (
            <article key={job.id} className="mb-3 rounded-lg border p-3">
              <div className="flex items-start gap-3">
                <LoaderCircle
                  className={cn(
                    "mt-0.5 size-4 text-muted-foreground",
                    job.status === "running" && "animate-spin",
                  )}
                />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap gap-x-2">
                    <span className="font-medium">{job.type}</span>
                    <span className="text-sm text-muted-foreground">
                      {jobStatusLabel(job.status)}
                    </span>
                  </div>
                  {job.progress && (
                    <p className="mt-1 text-xs text-muted-foreground">
                      {job.progress.message ??
                        `${job.progress.current ?? 0}/${job.progress.total ?? "?"}`}
                    </p>
                  )}
                  {job.error && (
                    <p className="mt-1 text-xs text-destructive">{job.error}</p>
                  )}
                </div>
                {["queued", "running"].includes(job.status) && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void cancel(job)}
                  >
                    取消
                  </Button>
                )}
                {job.status === "failed" && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void retry(job)}
                    disabled={retryingId === job.id}
                  >
                    {retryingId === job.id ? (
                      <LoaderCircle className="animate-spin" />
                    ) : (
                      <RefreshCw />
                    )}{" "}
                    重试
                  </Button>
                )}
              </div>
            </article>
          ))
        )}
      </section>
      <aside className="min-h-0 overflow-auto border-t p-4 md:border-t-0 md:border-l">
        <h2 className="mb-3 text-sm font-medium">实时事件</h2>
        {events.length === 0 ? (
          <p className="text-sm text-muted-foreground">正在等待事件...</p>
        ) : (
          events.map((event, index) => (
            <div
              key={`${event.id ?? "event"}-${index}`}
              className="mb-2 rounded border p-2 text-xs"
            >
              <span className="font-medium">{event.event}</span>
              <p className="mt-1 break-words text-muted-foreground">
                {eventText(event)}
              </p>
            </div>
          ))
        )}
      </aside>
    </div>
  );
}

function jobStatusLabel(status: string): string {
  return (
    (
      {
        queued: "排队中",
        running: "执行中",
        completed: "已完成",
        failed: "失败",
        cancelled: "已取消",
        interrupted: "已中断",
      } as Record<string, string>
    )[status] ?? status
  );
}

function SettingsView({
  client,
  onError,
  onNotice,
}: {
  client: ApiClient;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
}) {
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [draft, setDraft] = useState("{}");
  const [endpoint, setEndpoint] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiKeyConfigured, setApiKeyConfigured] = useState(false);
  const [loading, setLoading] = useState(true);
  const [savingChat, setSavingChat] = useState(false);
  const [saving, setSaving] = useState(false);

  const applySettings = useCallback((loaded: SettingsData) => {
    const webChat = webChatValues(loaded);
    setSettings(loaded);
    setDraft(JSON.stringify(safeSettingsForEdit(loaded), null, 2));
    setEndpoint(webChat.endpoint);
    setModel(webChat.model);
    setApiKey("");
    setApiKeyConfigured(webChat.apiKeyConfigured);
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void client.settings
      .get({ signal: controller.signal })
      .then(applySettings)
      .catch((error: unknown) => {
        if (!isAbortError(error)) onError(errorMessage(error));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [applySettings, client, onError]);

  async function saveWebChat() {
    if (!endpoint.trim() || !model.trim()) {
      onError("请填写 Web 对话的 Endpoint 和模型。");
      return;
    }
    setSavingChat(true);
    try {
      const updated = await client.settings.updateWebChat({
        endpoint: endpoint.trim(),
        model: model.trim(),
        apiKey,
      });
      applySettings(updated);
      onNotice("Web 对话设置已保存");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setSavingChat(false);
    }
  }

  async function save() {
    let patch: SettingsData;
    try {
      const parsed: unknown = JSON.parse(draft);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
        throw new Error("高级设置必须是 JSON 对象。");
      patch = parsed as SettingsData;
    } catch (error) {
      onError(errorMessage(error));
      return;
    }
    setSaving(true);
    try {
      const updated = await client.settings.update(patch);
      applySettings(updated);
      onNotice("高级设置已保存");
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      setSaving(false);
    }
  }

  if (loading) return <LoadingScreen label="正在加载设置..." />;
  return (
    <div className="grid h-full min-h-0 grid-cols-1 xl:grid-cols-2">
      <section className="min-h-0 overflow-auto border-b p-4 xl:border-r xl:border-b-0">
        <h1 className="mb-2 font-medium">Web 对话</h1>
        <p className="mb-4 text-sm text-muted-foreground">
          填写服务端调用 OpenAI 兼容接口所需的配置。API Key 只会提交，不会回显。
        </p>
        <div className="space-y-3 rounded-lg border p-3">
          <label
            className="block text-sm font-medium"
            htmlFor="web-chat-endpoint"
          >
            Endpoint
            <Input
              id="web-chat-endpoint"
              className="mt-1"
              value={endpoint}
              onChange={(event) => setEndpoint(event.target.value)}
              placeholder="https://api.example.com/v1/chat/completions"
            />
          </label>
          <label className="block text-sm font-medium" htmlFor="web-chat-model">
            模型
            <Input
              id="web-chat-model"
              className="mt-1"
              value={model}
              onChange={(event) => setModel(event.target.value)}
              placeholder="gpt-4o-mini"
            />
          </label>
          <label
            className="block text-sm font-medium"
            htmlFor="web-chat-api-key"
          >
            API Key（可选）
            <Input
              id="web-chat-api-key"
              className="mt-1"
              type="password"
              value={apiKey}
              onChange={(event) => setApiKey(event.target.value)}
              autoComplete="new-password"
              placeholder={
                apiKeyConfigured
                  ? "已配置；留空可保留当前密钥"
                  : "留空表示不设置"
              }
            />
            <span className="mt-1 block text-xs font-normal text-muted-foreground">
              {apiKeyConfigured
                ? "服务端已保存密钥；留空不会覆盖。"
                : "尚未配置密钥。"}
            </span>
          </label>
          <Button
            className="w-full"
            onClick={() => void saveWebChat()}
            disabled={savingChat}
          >
            {savingChat ? <LoaderCircle className="animate-spin" /> : <Save />}{" "}
            保存 Web 对话设置
          </Button>
        </div>
        <h2 className="mb-2 mt-6 font-medium">当前设置</h2>
        <div className="rounded-lg border">
          {Object.entries(settings ?? {}).length === 0 ? (
            <p className="p-3 text-sm text-muted-foreground">
              服务端未返回设置。
            </p>
          ) : (
            Object.entries(settings ?? {}).map(([key, value]) => (
              <div
                key={key}
                className="flex items-center gap-3 border-b px-3 py-2 last:border-b-0"
              >
                <span className="min-w-0 flex-1 truncate text-sm">{key}</span>
                <span className="max-w-40 truncate text-xs text-muted-foreground">
                  {isSensitiveSettingKey(key) ? "已配置" : listValue(value)}
                </span>
              </div>
            ))
          )}
        </div>
      </section>
      <section className="flex min-h-0 flex-col p-4">
        <div className="mb-2 flex items-center">
          <h2 className="font-medium">高级 JSON 设置</h2>
          <Button
            className="ml-auto"
            size="sm"
            onClick={() => void save()}
            disabled={saving}
          >
            {saving ? <LoaderCircle className="animate-spin" /> : <Save />} 保存
          </Button>
        </div>
        <p className="mb-3 text-sm text-muted-foreground">
          敏感字段不会显示在此处；如需更新密钥，请使用上方表单。
        </p>
        <textarea
          className="min-h-0 flex-1 resize-none rounded-lg border bg-background p-3 font-mono text-xs leading-5 outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          spellCheck={false}
          aria-label="高级设置 JSON"
        />
      </section>
    </div>
  );
}

function EmptyPanel({
  icon: Icon,
  label,
}: {
  icon: typeof FileText;
  label: string;
}) {
  return (
    <div className="flex h-full min-h-32 flex-col items-center justify-center gap-2 text-center text-sm text-muted-foreground">
      <Icon className="size-7 opacity-35" />
      <p>{label}</p>
    </div>
  );
}
