# LLM Wiki Web 端完整实现方案

**状态：** Web Preview 与 App 风格工作台已实现；高级导入、向量检索、Research 与 CLI 能力按后续阶段继续收口
**目标版本：** Web Edition v1
**设计日期：** 2026-09-05
**适用场景：** 在 Linux 开发机或服务器上运行 LLM Wiki，通过 `IP:port` 使用浏览器访问；项目文件位于服务器本地磁盘。

当前实现已经覆盖无窗口服务、登录与 CSRF、项目管理、文件 CRUD、上传与受保护资源预览、关键词搜索、WikiLink 图谱、Review、OpenAI-compatible Chat SSE、持久化 Job 状态、设置脱敏以及 systemd/Caddy 部署。完整桌面功能等价仍以后续阶段中的文档解析/自动 ingest、混合向量检索、Deep Research 和受控 CLI 为边界。

## 1. 结论与架构决策

LLM Wiki 可以改造成完整 Web 版，但不能仅把 Vite 暴露到 `0.0.0.0`。当前 React 前端同时承担 UI、工作流编排和部分持久化职责，并通过 Tauri IPC、插件和事件访问本机能力；Web 化需要建立独立的无窗口后端、受约束的 HTTP API 和浏览器运行时适配层。

本方案采用以下决策：

1. **单机、自托管、单用户优先。** 首版不建设 SaaS 多租户，但身份、资源所有权和审计接口不写死匿名用户，保留后续扩展空间。
2. **服务器磁盘是项目数据源。** 浏览器不直接访问客户端磁盘；新资料通过上传、URL 导入或服务器允许目录导入。
3. **一个公开服务。** Rust Server 同时提供 React 静态资源、REST API、SSE 和受保护的媒体资源；不引入常驻 Node 工作流服务。
4. **复用现有 Rust 核心。** 项目、文件、解析、搜索、向量、Agent、CLI 等能力从 Tauri 壳中抽出为无 Tauri 依赖的 core。
5. **分阶段迁移 TS 工作流。** 初期由浏览器继续编排已有 ingest、lint、dedup、deep-research 流程，快速实现功能覆盖；最终将需要后台持续、恢复和调度的流程迁入 Rust Job Runtime。一次性纯 Rust 重写和新增 Node 守护进程都不采用。[推断]
6. **业务能力统一走 HTTP。** 最终桌面版和 Web 版使用相同业务 API；Tauri 只保留目录选择、系统打开、托盘、开机启动等桌面专属能力。
7. **同源访问。** UI 和 API 通过同一 `scheme://host:port` 提供，不通过开放任意 CORS 解决部署问题。
8. **浏览器永不获得服务端密钥或宿主机绝对路径。** 所有文件请求使用 `projectId + relativePath`，LLM、Embedding、搜索和 MinerU 凭据只由服务端使用。
9. **完整 Web 版要求后台任务不依赖浏览器存活。** ingest、索引、Deep Research、定时导入等任务在浏览器关闭或刷新后继续执行，并可在重连后恢复查看。

**总体置信度：HIGH。**

---

## 2. 当前状态与主要差距

### 2.1 可直接复用的基础

- React/Vite 已能独立产出静态资源，入口由 `vite.config.ts` 和根目录 `package.json` 管理。
- Rust 已有本地 HTTP API，默认端口为 `19828`，已有项目列表、文件读取、Review、搜索、图谱、Source Watch 重扫、页面向量化、Agent Chat、SSE 和取消接口：`src-tauri/src/api_server.rs:22-41,342-369`。
- API 已支持从环境变量或配置决定绑定地址：`src-tauri/src/server_bind.rs:6-30`。
- Agent 已按 Desktop、HTTP API、MCP 共用核心组织：`src-tauri/src/agent/mod.rs:1-6`。
- MCP 已通过 HTTP API 复用项目、搜索和图谱能力，而不是直接访问磁盘：`mcp-server/README.md:3-6`。
- API 文件读取已有相对路径、canonical path 和符号链接逃逸防护基础：`src-tauri/src/api_server.rs:923-1003`。

### 2.2 当前阻断

- API Server 仍在 Tauri `setup()` 中启动，配置读取依赖 `AppHandle` 和 Tauri app data：`src-tauri/src/lib.rs:552-606`、`src-tauri/src/api_server.rs:701-727`。
- [计算] 生产前端约有 32 个文件直接依赖 Tauri，包含 67 个不同的 `invoke` 命令。
- 文件写入、目录、历史、项目生命周期、向量维护、CLI 和导入能力主要只有 Tauri command，没有完整 HTTP API：`src-tauri/src/lib.rs:624-702`。
- ingest 主流程位于前端 TypeScript，且依赖 Zustand、Tauri 文件操作、LLM client 和前端队列：`src/lib/ingest.ts`。
- 现有 CORS 只允许浏览器扩展、loopback 和 Tauri origin，不允许一般的 `http://<server-ip>:<port>` 跨源页面：`src-tauri/src/cors.rs:11-35`。
- 图片、PDF、音视频等预览依赖 Tauri asset protocol 和 `convertFileSrc`：`src-tauri/tauri.conf.json:24-29`、`src/components/editor/file-preview.tsx`、`src/lib/markdown-image-resolver.ts`。
- 当前通用文件写 command 接受绝对路径，不能直接映射为远程 HTTP 写接口：`src/commands/fs.ts:22-35`、`src-tauri/src/commands/fs.rs:1209-1306`。
- 全局设置和 API key 存在 Tauri Store `app-state.json`，浏览器不能继续直接读写：`src/lib/project-store.ts:1-14`。

---

## 3. 目标架构

```text
Browser
  |
  | HTTPS (remote) / HTTP (loopback only)
  v
+-------------------------------------------------------+
| llm-wiki-server                                      |
|                                                       |
|  Static UI  REST API  SSE  Auth  Media  Job Manager  |
|       |         |      |     |      |        |        |
|       +---------+------+-----+------+--------+        |
|                         |                             |
|                  llm-wiki-core                       |
|   Project / Safe FS / Parser / Search / Vector       |
|   Agent / History / Config / Provider / File Watch   |
+-------------------------+-----------------------------+
                          |
              server-local filesystem
       workspace roots / project dirs / app data

Optional adapters:
- Tauri desktop shell -> same core/business API
- MCP stdio server -> public API with MCP token
- Chrome extension -> clip API with extension token
```

### 3.1 目标代码结构

```text
Cargo.toml                       # Rust workspace
crates/
  llm-wiki-core/
    src/
      context.rs                 # BackendContext and dependency wiring
      config.rs                  # config repository and secret references
      projects.rs                # registry and project lifecycle
      paths.rs                   # ProjectPath and safe path resolution
      files.rs                   # read/write/copy/move/delete/assets
      history.rs
      search.rs
      vector.rs
      ingest.rs                  # target-state server workflow
      jobs.rs
      events.rs
      agent/
      providers/
  llm-wiki-server/
    src/
      main.rs
      router.rs
      auth.rs
      middleware.rs
      static_files.rs
      api/
  llm-wiki-desktop-adapter/      # optional; Tauri-specific bridge helpers
src-tauri/                       # desktop packaging and native-only UX
src/
  api/
    client.ts
    contracts.ts
    files.ts
    projects.ts
    jobs.ts
    chat.ts
  platform/
    runtime.ts
    native-capabilities.ts
    tauri-native.ts
    web-native.ts
```

实施时允许先在当前 `src-tauri` crate 内完成 `BackendContext` 解耦，再移动到 Cargo workspace，避免大爆炸重构；目标状态不得让 `llm-wiki-core` 依赖 Tauri。[推断]

### 3.2 组件职责

#### `llm-wiki-core`

- 不包含 HTTP、Tauri、窗口或 WebView 概念。
- 只接受领域参数，例如 `ProjectId`、`ProjectRelativePath`、`UserId`、`JobId`。
- 管理项目注册、受限文件系统、历史、解析、搜索、向量、Agent 和后台任务。
- 通过 `EventSink`、`ConfigRepository`、`SecretProvider` 等小接口注入运行环境。
- 现有 Tauri command 和 HTTP handler 都变成薄 adapter，不重复业务逻辑。

#### `llm-wiki-server`

- 使用 Tokio 异步运行时。
- 推荐将当前 `tiny_http` 路由迁移为 `axum + tower-http`。上传、SSE、中间件、静态资源、超时和优雅停机均属于完整 Web 服务的基础能力；继续扩展同步 `tiny_http` 会增加自定义基础设施代码。[推断]
- 提供一个公开端口，托管 `/`、`/api/v2/*`、`/assets/*`。
- SPA fallback 只处理非 API 的 `GET/HEAD` 请求；`/api`、`/assets` 和未知写请求必须返回真实 404/405，不能回退到 `index.html`。
- 管理认证、会话、CSRF、限流、请求 ID、审计和 Job Runtime。
- API handler 不直接拼接磁盘路径，只调用 core service。

#### React UI

- 所有业务访问集中到 `src/api/*`；组件和领域逻辑不得直接 import Tauri API。
- Web 和桌面共享 UI。
- `src/platform/*` 只保留无法跨端统一的 UX：目录选择、系统打开、窗口主题、开机启动。
- 浏览器侧 Zustand 仅保存视图状态和服务端状态缓存，不作为权威持久化源。

---

## 4. 数据与配置模型

### 4.1 项目内容

保持现有项目目录格式，不迁移用户 Markdown 和原始资料：

```text
project/
  purpose.md
  schema.md
  raw/
    sources/
    assets/
  wiki/
  .llm-wiki/
```

这样桌面版、备份工具和现有数据可继续使用。

### 4.2 服务端控制面数据

新增应用级 SQLite 数据库：

```text
$LLM_WIKI_DATA_DIR/server.db
```

建议表：

- `schema_migrations`
- `projects`
- `users`
- `sessions`
- `api_tokens`
- `settings`
- `jobs`
- `job_events`
- `audit_events`
- `idempotency_keys`

SQLite 只存控制面和任务元数据；Wiki 正文、来源文件和 LanceDB 仍保存在项目目录。使用 SQLite 是为了保证会话、任务状态、幂等和并发更新的事务一致性，不把内容系统改造成数据库产品。

### 4.3 密钥

- 环境变量和 systemd credentials 优先级最高。
- 可选本地 secret store 文件：`$LLM_WIKI_DATA_DIR/secrets.json`，权限强制为 `0600`。
- API 只返回：`configured`、`source`、`updatedAt`，不返回 key、token 或可逆掩码。
- 首次迁移从 Tauri `app-state.json` 提取密钥，原文件备份后清除明文；迁移必须可重跑且具备失败回滚。
- Web UI 的 `localStorage` 只允许保存主题、布局、最近视图等非敏感偏好。

### 4.4 工作区根目录与旧项目兼容

服务启动时配置一个主要工作区根目录：

```bash
llm-wiki-server \
  --workspace-root /data/wiki \
  --data-dir /var/lib/llm-wiki
```

规则：

- Web 新建项目只能在 primary workspace root 下创建。
- 普通 Web API 注册已有项目时只提交相对于 primary workspace root 的路径。
- 管理员不得通过普通 API 注册任意宿主机绝对路径。
- 项目注册表在服务端保存 canonical absolute path，但浏览器只看见 project ID 和展示名。
- 为兼容桌面版已注册在任意目录的项目，迁移器支持三种显式策略：
  1. `copy`：复制到 primary workspace root，校验后再切换注册记录；
  2. `move`：同文件系统原子移动，失败时回滚注册记录；
  3. `approve-in-place`：仅把该项目的 canonical root 加入精确 allowlist，不扩大为其父目录的通用权限。
- 每个旧项目产生迁移报告，包含来源、目标、策略、冲突、校验结果和回滚位置。未选择策略的项目保持未迁移状态，不静默隐藏或删除。

### 4.5 `.llm-wiki` 状态归属

普通文件 API 始终拒绝 `.llm-wiki`，内部状态必须迁移到专用 service/API：

| 现有状态 | 目标权威存储 | 访问方式 |
|---|---|---|
| `review.json` | 项目 `.llm-wiki`，由 ReviewService 独占写入 | Review API |
| `lint.json` | 项目 `.llm-wiki`，由 LintService 独占写入 | Lint API |
| Chat history/preferences | Agent SessionStore / 专用项目状态 | Chat Session API |
| `ingest-queue.json` | SQLite jobs；迁移完成后停止旧格式写入 | Jobs API |
| `file-change-queue.json` | SQLite jobs/source_changes | Source Changes API |
| ingest/cache/parsed metadata | 项目内部 repository | Ingest/Source API |
| history snapshots | 项目 HistoryService | History API |
| LanceDB | 项目 IndexService | Search/Index API |

迁移期浏览器工作流不得通过通用文件接口读写这些文件。最小 JobStore、Review、Lint、Chat 和 ingest-state API 必须先于 FS transport 切换交付。

---

## 5. 文件系统安全模型

所有文件 API 必须遵循以下流程：

1. 根据 `projectId` 在服务端解析项目根目录。
2. 拒绝绝对路径、空路径、NUL、`..`、Windows prefix 和根目录组件。
3. canonicalize 项目根及已存在目标。
4. 新文件 canonicalize 最近存在父目录。
5. 验证结果仍位于项目根目录内。
6. 默认拒绝符号链接；若未来允许，必须逐段验证目标仍在项目内。
7. 普通文件 API 不暴露 `.llm-wiki`；内部状态通过专用 API 访问。
8. 写入使用临时文件、`fsync`、原子 rename，并接入文件历史。
9. 编辑请求携带 `If-Match` 或 revision；不匹配返回 `409 Conflict`。
10. 上传限制单文件、单请求、项目总量和并发数；压缩包检查路径穿越、文件数量和解压后总大小。
11. 资源响应设置正确 MIME、`Content-Disposition`、`X-Content-Type-Options: nosniff`，媒体支持 Range。

不能直接把当前 `write_file`、`copy_file`、`delete_file` 等绝对路径 command 暴露为 HTTP API。

---

## 6. API 设计

### 6.1 通用约定

- 新 Web API 使用 `/api/v2`；现有 `/api/v1` 在迁移期保持 MCP 兼容。
- JSON 使用 camelCase。
- 成功响应直接返回资源；错误统一为：

```json
{
  "error": {
    "code": "FILE_REVISION_CONFLICT",
    "message": "File was modified by another client",
    "requestId": "req_...",
    "details": {}
  }
}
```

- 列表接口使用 `cursor`、`limit`，避免新增 offset 分页。
- 创建任务和上传接口支持 `Idempotency-Key`。
- 所有写接口写入审计日志。
- `GET /health/live` 只表示进程存活；`GET /health/ready` 检查数据库、workspace 和核心服务。

### 6.2 Auth 与系统

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/health/live` | 存活探针，无敏感信息 |
| GET | `/api/v2/health/ready` | 就绪探针 |
| POST | `/api/v2/auth/login` | 使用初始化 token/密码登录 |
| POST | `/api/v2/auth/logout` | 注销当前会话 |
| GET | `/api/v2/auth/session` | 当前用户及能力 |
| POST | `/api/v2/auth/csrf` | 刷新 CSRF token |
| GET | `/api/v2/system/capabilities` | Web/desktop、CLI、解析器等能力 |

浏览器登录后使用 `HttpOnly`、`SameSite=Strict` cookie；启用 TLS 时加 `Secure`。非 GET 请求校验 CSRF token 和 `Origin`。MCP、扩展和自动化客户端使用独立 Bearer token，不复用浏览器 cookie。

初始化规则：

- 不提供默认用户名或默认密码。
- 首次启动必须通过环境变量、systemd credential 或仅写入 `0600` 文件的一次性 bootstrap token 初始化管理员。
- 密码使用 Argon2id 保存摘要；bootstrap token 使用后立即失效。
- Session ID 每次登录轮换，服务端保存摘要，支持单会话注销和全量吊销。
- 直接监听非 loopback 时必须同时配置认证和服务端 TLS；反向代理部署下 Rust 后端保持 loopback。

### 6.3 设置与凭据

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/settings` | 获取脱敏设置 |
| PATCH | `/api/v2/settings` | 更新非敏感设置 |
| PUT | `/api/v2/secrets/{provider}` | 写入/轮换 provider secret |
| DELETE | `/api/v2/secrets/{provider}` | 删除 secret |
| POST | `/api/v2/providers/{provider}/test` | 服务端测试连接 |
| GET | `/api/v2/integrations/status` | MCP、clip、CLI、MinerU 状态 |

桌面专属设置如 `closeBehavior`、`autostart` 不出现在 Web 设置页；共享设置由服务端保存，主题和缩放由浏览器本地保存。

### 6.4 模型与外部服务 Gateway

迁移期浏览器仍会运行部分 TypeScript 工作流，因此需要受约束的服务端调用接口：

| Method | Path | 用途 |
|---|---|---|
| POST | `/api/v2/runtime/llm/chat` | 按 profile/task type 调用 LLM，支持流式响应 |
| POST | `/api/v2/runtime/embeddings` | 服务端生成 embedding |
| POST | `/api/v2/runtime/web-search` | 使用已配置搜索提供商 |
| POST | `/api/v2/runtime/anytxt-search` | 调用已配置 AnyTXT 服务 |
| POST | `/api/v2/runtime/mineru/parse` | 调用已配置 MinerU 服务或创建解析 job |

这些接口只接受 profile ID、任务类型和业务 payload；provider URL、API key、自定义鉴权头和代理配置由服务端读取。禁止提供可转发任意 URL、任意 method 和任意 header 的通用代理接口。接口仅供同源 UI 使用，不默认授予 MCP/extension token。

当 ingest、Deep Research 等工作流迁入 Job Runtime 后，浏览器不再直接调用其底层 runtime endpoint；未被其他功能使用的接口应删除，而不是永久保留双轨。

### 6.5 项目

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/projects` | 项目列表 |
| POST | `/api/v2/projects` | 在 workspace root 下新建项目 |
| POST | `/api/v2/projects/register` | 注册 workspace root 下已有项目 |
| GET | `/api/v2/projects/{projectId}` | 项目详情与能力 |
| PATCH | `/api/v2/projects/{projectId}` | 重命名、项目配置 |
| DELETE | `/api/v2/projects/{projectId}` | 从注册表移除；默认不删除磁盘内容 |
| POST | `/api/v2/projects/{projectId}/archive` | 导出项目归档任务 |
| POST | `/api/v2/projects/import` | 上传并导入项目归档任务 |

Web 不保留“当前项目”全局状态；当前项目由浏览器 URL 和会话决定，例如 `/projects/{projectId}/wiki/...`。这样多个标签页可以安全打开不同项目。

### 6.6 文件、目录与资源

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/projects/{id}/tree` | 获取目录树，可传 depth/includeHidden |
| GET | `/api/v2/projects/{id}/files/content?path=` | 获取文本及 revision |
| PUT | `/api/v2/projects/{id}/files/content?path=` | 原子覆盖文本，要求 revision |
| PATCH | `/api/v2/projects/{id}/files/content?path=` | 选择区编辑或结构化 patch |
| DELETE | `/api/v2/projects/{id}/files?path=` | 删除文件/目录 |
| POST | `/api/v2/projects/{id}/directories` | 创建目录 |
| POST | `/api/v2/projects/{id}/files/move` | 移动/重命名 |
| POST | `/api/v2/projects/{id}/files/copy` | 复制 |
| POST | `/api/v2/projects/{id}/uploads` | multipart 上传来源文件 |
| GET | `/api/v2/projects/{id}/assets?path=` | 图片、PDF、音视频流/下载 |
| GET | `/api/v2/projects/{id}/files/metadata?path=` | 大小、mtime、hash、revision |
| POST | `/api/v2/projects/{id}/pages/missing` | 创建缺失 Wiki 页面 |
| GET | `/api/v2/projects/{id}/pages/{path}/links` | 页面链接与反向链接 |

浏览器预览统一使用 `/assets`，替换 `convertFileSrc` 和 Base64 整文件读取。PDF 和媒体优先流式读取，不把大文件完整编码进 JSON。

### 6.7 历史

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/projects/{id}/history/settings` | 历史配置 |
| PATCH | `/api/v2/projects/{id}/history/settings` | 更新配置 |
| GET | `/api/v2/projects/{id}/history?path=` | 文件历史列表 |
| GET | `/api/v2/projects/{id}/history/{entryId}` | 获取版本内容/元数据 |
| POST | `/api/v2/projects/{id}/history/{entryId}/restore` | 恢复版本 |
| GET | `/api/v2/projects/{id}/history/stats` | 历史统计 |
| DELETE | `/api/v2/projects/{id}/history` | 清理历史 |

所有 Web 编辑、Agent 写入、导入和自动修复必须使用同一历史服务。

### 6.8 搜索、图谱和索引

| Method | Path | 用途 |
|---|---|---|
| POST | `/api/v2/projects/{id}/search` | 关键词/向量/图谱混合搜索 |
| GET | `/api/v2/projects/{id}/graph` | 获取图谱 |
| GET | `/api/v2/projects/{id}/index/status` | 索引状态与统计 |
| POST | `/api/v2/projects/{id}/index/rebuild` | 创建重建任务 |
| POST | `/api/v2/projects/{id}/index/optimize` | 创建优化任务 |
| POST | `/api/v2/projects/{id}/pages/embed` | 单页增量向量化 |

不公开底层 `vector_upsert`、`vector_clear` 等原始接口，防止客户端破坏索引一致性。

### 6.9 Sources、导入和文件监听

| Method | Path | 用途 |
|---|---|---|
| POST | `/api/v2/projects/{id}/sources/import` | 上传、URL 或服务器允许路径导入 |
| GET | `/api/v2/projects/{id}/sources` | 来源列表和处理状态 |
| GET | `/api/v2/projects/{id}/sources/{sourceId}` | 来源详情 |
| DELETE | `/api/v2/projects/{id}/sources/{sourceId}` | 删除来源并触发生命周期处理 |
| POST | `/api/v2/projects/{id}/sources/rescan` | 创建重扫任务 |
| GET | `/api/v2/projects/{id}/source-changes` | 文件变更队列 |
| POST | `/api/v2/projects/{id}/source-changes/{taskId}/retry` | 重试 |
| POST | `/api/v2/projects/{id}/source-changes/{taskId}/ignore` | 忽略 |
| GET | `/api/v2/projects/{id}/schedules` | 计划导入列表 |
| POST | `/api/v2/projects/{id}/schedules` | 创建计划 |
| PATCH | `/api/v2/projects/{id}/schedules/{scheduleId}` | 更新计划 |
| DELETE | `/api/v2/projects/{id}/schedules/{scheduleId}` | 删除计划 |

浏览器上传文件进入服务器 staging 目录，通过校验后再原子移动到 `raw/sources`。URL 导入必须执行 SSRF 防护；服务器目录导入只能访问配置的 workspace/import roots。

### 6.10 Agent、Chat 和 Skills

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/projects/{id}/chat/sessions` | 会话列表 |
| POST | `/api/v2/projects/{id}/chat/sessions` | 新建会话 |
| GET | `/api/v2/projects/{id}/chat/sessions/{sessionId}` | 会话详情 |
| DELETE | `/api/v2/projects/{id}/chat/sessions/{sessionId}` | 删除会话 |
| POST | `/api/v2/projects/{id}/chat/sessions/{sessionId}/turns` | 创建 Agent turn job |
| POST | `/api/v2/projects/{id}/chat/sessions/{sessionId}/cancel` | 取消当前 turn |
| GET | `/api/v2/projects/{id}/skills` | 技能列表 |
| POST | `/api/v2/projects/{id}/skills/{skillId}/enable` | 启用技能 |
| POST | `/api/v2/projects/{id}/skills/{skillId}/disable` | 禁用技能 |
| POST | `/api/v2/jobs/{jobId}/approvals` | 审批精确 shell/tool 动作 |

现有 Agent SSE 响应可在迁移阶段继续使用；目标状态统一到 Job Event Stream。

### 6.11 Review、Lint、Research

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/projects/{id}/reviews` | Review 列表 |
| PATCH | `/api/v2/projects/{id}/reviews/{reviewId}` | 更新状态 |
| POST | `/api/v2/projects/{id}/reviews/resolve` | 批量处理 |
| GET | `/api/v2/projects/{id}/lint` | Lint 状态 |
| POST | `/api/v2/projects/{id}/lint/run` | 创建 Lint job |
| POST | `/api/v2/projects/{id}/lint/{itemId}/fix` | 创建修复 job |
| GET | `/api/v2/projects/{id}/research` | Research 任务列表 |
| POST | `/api/v2/projects/{id}/research` | 创建 Deep Research job |
| POST | `/api/v2/projects/{id}/research/{jobId}/cancel` | 取消 |

### 6.12 Jobs 与事件

所有长任务统一为 job：

```json
{
  "id": "job_...",
  "projectId": "...",
  "type": "source.ingest",
  "status": "queued",
  "progress": { "current": 0, "total": 12 },
  "createdAt": "...",
  "updatedAt": "..."
}
```

接口：

| Method | Path | 用途 |
|---|---|---|
| GET | `/api/v2/jobs?projectId=&status=` | 任务列表 |
| GET | `/api/v2/jobs/{jobId}` | 任务状态 |
| GET | `/api/v2/jobs/{jobId}/events` | SSE 任务事件 |
| POST | `/api/v2/jobs/{jobId}/cancel` | 请求取消 |
| POST | `/api/v2/jobs/{jobId}/retry` | 重试失败任务 |
| GET | `/api/v2/events?projectId=` | 项目级合并 SSE 流 |

事件必须包含递增 `eventId`，支持 `Last-Event-ID` 重连。服务重启后从 SQLite 恢复 `queued/running` 任务；无安全断点的任务标为 `interrupted` 并按策略重试，而不是伪装成功。

---

## 7. 前端迁移设计

### 7.1 Transport 边界

新增统一接口：

```ts
interface BackendTransport {
  projects: ProjectApi
  files: FileApi
  settings: SettingsApi
  search: SearchApi
  jobs: JobApi
  agent: AgentApi
  integrations: IntegrationApi
}
```

约束：

- React component 不允许直接调用 `invoke()`。
- 除 `src/platform/tauri-*` 外，不允许 import `@tauri-apps/*`。
- API DTO 与 UI model 分离；DTO 由 OpenAPI 或 JSON Schema 生成类型。
- `AbortSignal` 统一映射到 HTTP abort 或 Tauri command cancel。

### 7.2 桌面能力替换

| 当前能力 | Web 替代 |
|---|---|
| Tauri directory dialog | workspace root 下创建/注册；文件使用 `<input type=file>` 上传 |
| Tauri Store | 服务端 settings；主题/布局使用 localStorage |
| `convertFileSrc` | 鉴权 `/assets?path=` URL |
| opener | 外部 URL 使用 `window.open`；服务器文件使用预览/下载 |
| autostart/close behavior | Web 隐藏；由 systemd 管理服务启动 |
| plugin-http | 服务端 provider/search/MinerU gateway |
| Tauri event | SSE |
| CLI process event | server job + SSE |
| `127.0.0.1:19827` clip polling | 同源 clip API + job event |

### 7.3 工作流迁移顺序

1. `commands/fs.ts` 和 `file-sync.ts` 改为 transport adapter。
2. 项目、Wiki 编辑、文件树、搜索、图谱、Review、Chat 改用 HTTP。
3. `tauri-fetch.ts` 改为服务端 provider gateway，不在浏览器拼接 API key。
4. 预览和 Markdown 图片改用 asset API。
5. ingest 暂时保留 TS 编排，但所有副作用走 HTTP；状态写入 server job record。
6. 将 ingest 迁入 Rust Job Runtime。
7. 迁移 deep-research、scheduled-import、dedup/reindex、lint/fix。
8. 删除迁移期浏览器编排和重复 v1 写接口，不长期维护双轨。

---

## 8. 后台任务执行模型

### 8.1 必须后端化的流程

- Source ingest 和文档预处理
- 图片提取与 caption
- Embedding 和索引重建
- Source Watch 和计划导入
- Deep Research
- Dedup、sweep、lint/fix
- 项目导入导出
- Claude/Codex CLI 和 Agent shell

### 8.2 并发和一致性

- 每个 canonical project root 使用跨进程 advisory lock，锁文件位于 `.llm-wiki/write-owner.lock`；独立 Server 和更新后的 Tauri 写路径必须使用同一锁协议。
- Phase 2 开放 Web 写入前，桌面端写 command 必须接入该锁；旧版本桌面端不具备此协议，因此升级说明必须禁止其与 Web Server 同时打开同一项目。
- `data-dir` 锁只防止控制面双开，不能替代 project-root 锁。
- 同一来源文件最多一个 ingest job。
- 索引更新以页面 revision 为条件，过期结果丢弃。
- 编辑保存使用 revision/ETag，冲突由用户选择刷新或合并。
- Job cancel 使用 cancellation token；外部请求、子进程和循环必须检查取消。
- 服务端统一产生 Activity/Review/History 记录，浏览器只订阅。
- 多标签页通过 SSE 接收文件、任务、Review 和索引变化。

---

## 9. 安全设计

### 9.1 网络和认证

- 默认绑定 `127.0.0.1`。
- 非 loopback 访问必须满足其一：Rust 配置 `--tls-cert/--tls-key` 直接提供 HTTPS；或 Rust 只监听 loopback，由受信 Caddy/Nginx 提供 HTTPS。
- 非 loopback 明文 HTTP 默认拒绝启动，即使已经配置密码也不例外；本地开发应使用 SSH port forwarding 或显式的测试模式，测试模式不得承载真实密钥。
- 推荐 Caddy/Nginx 终止 TLS，Rust 服务继续监听 loopback。
- 只信任显式配置代理的 `X-Forwarded-*`。
- 浏览器 cookie 与 MCP/extension token 分离，可独立吊销和轮换。
- 删除 query-string token 支持；当前 `/api/v1` 的 query token 仅在兼容期保留并输出弃用警告。
- 登录、provider test、上传、Chat、CLI 分别限流；限流键至少包含身份和来源 IP。

### 9.2 浏览器安全

- 校验 `Origin` 和 CSRF。
- CSP 默认 `default-src 'self'`，只为必要资源增加精确来源。
- Markdown HTML 默认不执行；外部链接增加 `noopener noreferrer`。
- 用户提供 HTML、SVG、附件采用下载或隔离 sandbox origin，不在主应用 origin 执行。
- 错误响应不包含绝对路径、API key、完整命令环境和 Rust backtrace。

### 9.3 SSRF 和第三方请求

- 浏览器不能提交任意目标 URL 给通用代理。
- Provider endpoint 只能来自管理员保存的配置。
- 默认拒绝 URL 导入访问 loopback、link-local、云 metadata 和私网地址；如需本地模型，必须在管理员配置中显式允许目标 host/CIDR。
- 重定向后重新校验目标地址。
- 限制响应体大小、超时和重定向次数。

### 9.4 CLI 与 Shell

- Web 首次发布默认禁用 Agent shell、Claude CLI 和 Codex CLI。
- 启用 CLI 需要服务端配置，不接受浏览器指定 executable path。
- 子进程 CWD 固定到项目或 agent workspace，环境变量使用 allowlist。
- Shell 审批绑定 `userId + sessionId + jobId + exactCommandHash`，一次有效并设置短期过期时间。
- 所有启动、审批、取消和退出码写审计日志。
- 不提供通用远程 shell endpoint。

---

## 10. Clip、MCP 和外部集成

### 10.1 Clip Server

将当前独立 `19827` 服务并入主 API：

- `POST /api/v2/projects/{id}/clips`
- `GET /api/v2/projects/{id}/clips/{clipId}`
- clip 创建后直接产生持久化 ingest job。
- 使用 extension 专用 token 和 Origin allowlist。
- 迁移期保留 `19827` adapter，内部转发到新服务，稳定后删除。

### 10.2 MCP

- 继续使用 stdio MCP server，但默认 API base URL 指向统一 Web Server。
- MCP token 仅授予显式 scopes，例如 `projects:read`、`files:read`、`search`、`chat`。
- 现有 `/api/v1` 在 MCP 升级完成前保持兼容。
- 不在 Web v1 中引入远程 HTTP MCP transport，避免扩大首发范围。

---

## 11. 部署方案

### 11.1 服务启动

目标命令：

```bash
llm-wiki-server \
  --host 127.0.0.1 \
  --port 19828 \
  --workspace-root /data/llm-wiki/projects \
  --data-dir /data/llm-wiki/state
```

局域网直接访问使用服务端 TLS：

```bash
llm-wiki-server \
  --host 0.0.0.0 \
  --port 19828 \
  --tls-cert /etc/llm-wiki/server.crt \
  --tls-key /etc/llm-wiki/server.key
```

若不配置服务端 TLS，应保持监听 `127.0.0.1`，通过 Caddy/Nginx HTTPS、SSH port forwarding 或提供传输加密的 VPN 接入。不得把带真实凭据的明文 HTTP 当作受支持的远程部署方式。

### 11.2 systemd

```ini
[Unit]
Description=LLM Wiki Web Server
After=network-online.target

[Service]
User=llm-wiki
Group=llm-wiki
ExecStart=/opt/llm-wiki/bin/llm-wiki-server \
  --host 127.0.0.1 \
  --port 19828 \
  --workspace-root /srv/llm-wiki/projects \
  --data-dir /var/lib/llm-wiki
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/srv/llm-wiki/projects /var/lib/llm-wiki

[Install]
WantedBy=multi-user.target
```

若启用 CLI，需要按实际二进制和目录调整 systemd sandbox，不能直接关闭全部保护。

### 11.3 反向代理

推荐：

```text
https://wiki.example.internal/
  -> http://127.0.0.1:19828/
```

要求：

- TLS
- SSE 禁用代理缓冲
- 上传大小与服务端一致
- 较长的 ingest/chat read timeout
- 不缓存 API、会话和私有 asset

### 11.4 备份与恢复

备份集合：

- workspace root 下全部项目
- `server.db`
- secret store 或 systemd credential 来源
- 版本与 schema migration 信息

恢复顺序：停止服务、恢复文件、运行只读 migration check、启动服务、执行项目和索引一致性检查。LanceDB 可配置为可重建数据，但不能在未提示用户时自动删除。

---

## 12. 兼容与迁移

### 12.1 数据迁移

- 首次启动检测 Tauri `app-state.json`。
- 导入项目注册表和非敏感设置到 SQLite。
- 对 workspace root 外的项目逐项执行 `copy`、`move`、`approve-in-place` 或 `skip`，生成可回滚报告。
- 密钥迁入 secret provider。
- 幂等迁移 `ingest-queue.json`、`file-change-queue.json`、scheduled-import 配置以及遗留 Chat/Review/Lint 状态。
- 旧 `processing` 状态不得直接视为成功：根据来源 hash、目标 revision 和任务类型映射为 `queued`、`interrupted` 或 `completed`。
- 队列迁移与 migration marker 在同一事务提交；项目文件变更先写 staging，验证后再更新注册记录。
- 若不迁移未完成任务，升级前必须强制排空并向用户输出清单，不能静默丢弃。
- 保留时间戳备份和 migration marker。
- 项目目录结构保持不变。
- `data-dir` 使用进程锁；每个项目另使用 canonical root 跨进程锁。迁移期禁止旧桌面版本与 Web Server 同时写同一项目。

### 12.2 API 迁移

- `/api/v1` 保持现有 MCP 能力。
- Web 新功能只进入 `/api/v2`。
- MCP 切换 v2 后，为 v1 设置明确删除版本，不增加长期 fallback。
- 前端每迁移一个模块，就增加 transport contract test，并删除该模块的直接 Tauri import。

### 12.3 桌面版

- 第一阶段保持现有 Tauri command 行为。
- core 抽取后，Tauri command 调用 core service。
- Web API 稳定后，桌面 React 业务也切换统一 API。
- 最终只保留少量 native bridge：dialog、opener、window、tray、autostart。

---

## 13. 实施阶段与验收门槛

### Phase 0：契约冻结和安全基线

交付：

- 当前 `/api/v1` OpenAPI/JSON Schema。
- Tauri command 与 event 清单。
- 路径安全、认证、SSE 基线测试。
- CI 使用兼容 protoc；当前开发机 `protoc 3.7.0` 无法编译 Lance 依赖使用的 proto3 optional 参数。

验收：现有桌面行为、MCP API 和前端测试无回归。

### Phase 1：Core 解耦和 Headless Server

交付：

- `BackendContext`、配置仓储、项目仓储、事件总线。
- 独立 `llm-wiki-server`。
- 静态 `dist` 同源托管。
- 最小 SQLite JobStore/EventStore，以及 Review、Lint、Chat、ingest-state 专用 API；此阶段可以只记录 foreground job，不要求所有任务已后端执行。
- health、auth、项目读取、文件读取、搜索、图谱、Chat SSE。

验收：没有图形环境时可启动，浏览器可通过 IP:port 完成只读、搜索、图谱和 Chat。

### Phase 2：编辑、设置和资源预览

交付：

- 项目创建/注册和旧项目 root 迁移工具。
- 文本 CRUD、revision、历史、canonical project-root 跨进程锁。
- 上传、下载、图片/PDF/媒体预览。
- 脱敏设置、服务端 provider gateway。
- 前端主要 Tauri FS 调用迁入 HTTP transport。

验收：浏览器可创建项目、编辑 Wiki、上传来源、配置模型并预览资源；浏览器网络响应中不存在密钥和宿主绝对路径；新桌面端与 Server 不能同时取得同一项目写锁，旧桌面端并发写入场景被升级检查阻断或明确拒绝支持。

### Phase 3：导入和持久化 Jobs

交付：

- Job 表、Job Manager、SSE、取消、重试、恢复。
- ingest、预处理、图片 caption、embedding 后端化。
- Source Watch、变更队列和计划导入后端化。
- clip 服务并入主 API。

验收：关闭浏览器后任务继续；服务重启后任务可恢复或明确标记 interrupted；同一来源不会重复并发 ingest。

### Phase 4：高级工作流

交付：

- Deep Research、lint/fix、dedup/sweep、索引维护后端化。
- Chat session、Skills 和审批 API。
- 多标签页实时同步。

验收：桌面现有主要工作流在 Web 中达到行为等价，任务状态和失败原因可追踪。

### Phase 5：CLI、安全加固和发布

交付：

- 可选 Claude/Codex CLI job。
- 精确 shell 审批、审计、限流、SSRF 防护。
- systemd、反向代理示例、升级和备份文档。
- 删除不再需要的迁移适配和旧代码。

验收：安全测试、E2E、升级/回滚演练通过；默认配置无匿名 LAN 写入或命令执行能力。

---

## 14. 测试策略

### Rust core

- 项目注册和 workspace root 限制。
- 路径穿越、符号链接逃逸、TOCTOU 和 zip-slip。
- 原子写、revision 冲突和历史。
- Job 状态机、取消、重试、服务重启恢复。
- Agent/CLI 权限与审计。

### HTTP 集成测试

- 未登录、过期会话、CSRF、错误 Origin。
- Bearer scope。
- 上传限额、Range、MIME、缓存头。
- SSE 顺序、重连、慢客户端和背压。
- 每条写接口的项目隔离。
- 反向代理头和请求超时。

### Frontend

- HTTP transport contract tests。
- 禁止直接 Tauri import 的 lint rule。
- MSW/API mock component tests。
- Playwright E2E：登录、创建项目、上传、编辑冲突、搜索、Chat、任务恢复。
- Desktop 和 Web 的共享功能 parity tests。

### 发布验证

- 无 DISPLAY/WAYLAND 环境启动。
- `127.0.0.1` HTTP、`0.0.0.0` HTTPS 和 loopback + 反向代理三种模式。
- Chromium/Chrome、Firefox 最新稳定版。
- 旧项目、含大文件项目、包含符号链接项目。
- 浏览器刷新、断网重连、服务重启、磁盘空间不足。

---

## 15. 可观测性与运维

- 结构化日志包含 `requestId`、`userId`、`projectId`、`jobId`，不得包含密钥和完整正文。
- `/health/ready` 报告数据库、workspace、可选 provider 的分类状态，但不输出凭据。
- Job 记录阶段、耗时、重试次数和最终错误分类。
- 指标至少包含请求延迟、状态码、活跃 SSE、job 队列长度、失败率、provider 延迟和索引耗时。
- 审计日志记录登录、设置修改、文件写删、项目注册、token 管理、CLI/shell 审批。
- 日志轮转和保留由 systemd/journald 或部署环境负责。

---

## 16. 风险、控制和回滚

| 风险 | 控制 | 回滚方式 |
|---|---|---|
| Core 抽取引入桌面回归 | 先加契约测试，adapter 薄化，不同时改业务语义 | Tauri command 继续走旧实现直到单模块验证通过 |
| 浏览器编排任务在关闭后中断 | 迁移期明确标识 foreground job；完整发布前后端化关键任务 | 按工作流 feature flag 切回前端编排 |
| 任意文件读写 | projectId + relative path、canonicalize、workspace root、无通用 FS API | 禁用写路由，保留只读模式 |
| 密钥泄漏 | 服务端 secret provider、响应脱敏、同源 CSP | 禁用对应 provider 并轮换凭据 |
| 多标签页覆盖编辑 | revision/ETag 和项目事件流 | 冲突时只读并要求刷新/合并 |
| CLI 远程代码执行 | 默认关闭、固定二进制、精确审批、审计、systemd sandbox | 运行时关闭 CLI capability |
| API v1/v2 长期双轨 | 明确迁移窗口和删除版本 | MCP 暂时回退 v1；不回退数据格式 |
| SQLite 或迁移失败 | migration transaction、启动前备份、只读检查 | 恢复 `server.db` 和 app-state 备份 |

---

## 17. 工作量与发布切分

以下为单名熟悉代码库的工程师估算，不包含产品交互大改和外部安全审计：[推断]

| 阶段 | 估算 |
|---|---:|
| Phase 0 | 3-5 人日 |
| Phase 1 | 8-12 人日 |
| Phase 2 | 10-15 人日 |
| Phase 3 | 12-20 人日 |
| Phase 4 | 10-18 人日 |
| Phase 5 | 8-12 人日 |
| 合计 | 51-82 人日 |

建议发布两个里程碑：

1. **Web Preview：** Phase 0-2，具备项目管理、阅读、编辑、上传、搜索、图谱和 Chat。
2. **Web GA：** Phase 3-5，任务后台化、完整工作流、安全和运维能力完成。

不建议把“完整功能一次上线”作为单个不可拆分交付，因为当前前端工作流和 Tauri IPC 耦合面较大，分阶段能显著降低回归和数据损坏风险。[推断]

---

## 18. Definition of Done

Web Edition v1 只有在以下条件全部满足时才算完成：

- Linux 无图形环境可通过 systemd 启动。
- 用户可通过配置的 HTTPS `IP:port` 或 HTTPS 域名访问；非 loopback 明文 HTTP 不属于生产支持范围。
- 项目创建、注册、阅读、编辑、上传、预览、搜索、图谱、Review、Lint、Research 和 Chat 可用。
- ingest、索引、定时导入和 Research 不依赖浏览器持续打开。
- 浏览器不接收服务端密钥或宿主绝对路径。
- 所有文件写入受项目边界、revision 和历史保护。
- SSE 可重连，任务可取消，服务重启后状态可恢复或明确失败。
- 默认不允许匿名 LAN 写入，不默认开放 CLI/shell。
- MCP 和 Clip 已切到统一服务，旧接口有明确下线计划。
- 桌面版共享功能回归通过。
- 路径、认证、CSRF、SSRF、上传、CLI、并发写和迁移测试通过。
- 部署、备份、恢复、升级、回滚文档齐全。
