export interface ApiErrorBody {
  code?: string;
  message?: string;
  requestId?: string;
  details?: unknown;
}

export interface ApiErrorResponse {
  error?: ApiErrorBody;
}

export interface SessionUser {
  id: string;
  name?: string;
  email?: string;
  role?: string;
}

export interface Session {
  authenticated: boolean;
  kind?: "session" | "bearer";
  user?: SessionUser;
  capabilities?: string[];
  csrfToken?: string;
}

export interface CsrfToken {
  token?: string;
  csrfToken?: string;
}

export interface Project {
  id: string;
  name: string;
  createdAt?: number;
}

export interface ProjectListResponse {
  items?: Project[];
  projects?: Project[];
}

export interface CreateProjectInput {
  name: string;
  directoryName?: string;
}

export interface RegisterProjectInput {
  relativePath: string;
  name?: string;
}

export interface UpdateProjectInput {
  name: string;
}

export interface FileTreeNode {
  name: string;
  path: string;
  kind: "file" | "directory";
  isDir: boolean;
  children?: FileTreeNode[];
  mimeType?: string;
  size?: number;
  modifiedAt?: number;
}

/** Raw tree entries returned by the server before client normalization. */
export interface FileTreeNodeResponse {
  name: string;
  path: string;
  kind?: "file" | "directory";
  isDir?: boolean;
  is_dir?: boolean;
  children?: FileTreeNodeResponse[];
  mimeType?: string;
  size?: number;
  modifiedAt?: number;
}

export interface FileTreeResponse {
  items?: FileTreeNodeResponse[];
  tree?: FileTreeNodeResponse[];
  entries?: FileTreeNodeResponse[];
}

export interface FileContent {
  path: string;
  content: string;
  revision: string;
  mimeType?: string;
  updatedAt?: number;
}

export interface SaveFileInput {
  content: string;
  revision: string;
}

export interface MoveFileInput {
  sourcePath: string;
  targetPath: string;
}

export interface CreateDirectoryInput {
  path: string;
}

export interface CreateDirectoryResponse {
  path: string;
}

export interface UploadResponse {
  items: Array<{
    path: string;
    revision: string;
    size: number;
  }>;
}

export interface SearchResult {
  path: string;
  title: string;
  snippet?: string;
  score?: number;
  type?: string;
}

export interface SearchResponse {
  results?: SearchResult[];
  items?: SearchResult[];
}

export interface GraphNode {
  id: string;
  label?: string;
  path?: string;
  type?: string;
  linkCount?: number;
}

export interface GraphEdge {
  id?: string;
  source: string;
  target: string;
  weight?: number;
}

export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface ReviewOption {
  label: string;
  action: string;
}

export interface ReviewItem {
  id: string;
  title: string;
  description?: string;
  type?: string;
  status?: string;
  resolved?: boolean;
  options?: ReviewOption[];
  createdAt?: string;
  sourcePath?: string;
}

export interface ReviewListResponse {
  items?: ReviewItem[];
  reviews?: ReviewItem[];
}

export interface ChatSession {
  id: string;
  title?: string;
  projectId?: string;
  updatedAt?: number;
  createdAt?: number;
}

export interface ChatMessage {
  id?: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  createdAt?: number;
}

export interface ChatSessionDetail extends ChatSession {
  messages?: ChatMessage[];
}

export interface ChatSessionListResponse {
  items?: ChatSession[];
  sessions?: ChatSession[];
}

export interface JobProgress {
  current?: number;
  total?: number;
  message?: string;
}

export interface Job {
  id: string;
  projectId?: string;
  type: string;
  status: string;
  progress?: JobProgress;
  createdAt?: number;
  updatedAt?: number;
  error?: string;
}

export interface JobListResponse {
  items?: Job[];
  jobs?: Job[];
}

export interface WebChatSettingsInput {
  endpoint: string;
  model: string;
  /** Omit to retain the server-side secret. */
  apiKey?: string;
}

export type Settings = Record<string, unknown>;

export interface SseEvent {
  event: string;
  data: string;
  id?: string;
  retry?: number;
}
