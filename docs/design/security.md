# 安全模型

本文是信任边界、control socket、Profile Validation、终端安全、明文数据生命周期、日志与 Debug Capture 的唯一事实来源。

## 信任边界

```text
nono supervisor
    ├── trusted enforcement and validation
    └── sends approvable request
             │
             ▼
nono-approval daemon
    ├── trusted to faithfully return user's decision
    ├── not trusted to weaken nono hard denies
    └── never executes the requested operation itself
```

daemon 不判断操作是否安全，不覆盖 nono 的 deny、protected roots 或平台 sandbox 约束，只提供一次性人工决定。

## Webhook 与 Control 分离

- webhook ingress：固定 loopback TCP endpoint `/v1/webhooks/approval`；
- control：owner-only Unix socket，不开放 TCP 管理端口。

loopback webhook 没有 peer UID，也不认证 caller。任意本地进程都可以提交伪造请求、骚扰审批界面或占用 pending 容量；daemon 不执行请求中的操作，批准只返回给该精确 webhook 连接，因此不能借伪造请求批准或执行另一条真实操作。该 ingress 风险作为本地 fail-closed 可用性限制接受；control socket 才使用 OS peer credential 隔离其他用户。

## Control Socket

runtime 基础目录由 `directories::ProjectDirs` 解析，项目子目录权限为 `0700`，socket 权限为 `0600`。启动时若已有 socket：

- 先验证 runtime path 的 owner、类型与权限；
- 仅在确认目标是当前用户拥有的 socket 且没有活跃 daemon 时清理 stale socket；
- 普通文件、symlink、错误 owner 或不安全父目录一律启动失败；
- 不使用递归删除或宽泛路径清理。

socket pathname 必须能完整放入目标平台的 `sockaddr_un.sun_path`；超长路径启动失败。不得回退到共享 `/tmp` 或其他削弱 owner-only 边界的位置。

每条 control connection 都必须验证 peer UID：

- Linux：`SO_PEERCRED`；
- macOS：`LOCAL_PEERPID` 加 `getpeereid`；
- 无法获取或验证时 fail closed，不能退化为只看文件权限。

平台 adapter 使用 `nix` 的安全封装读取这些内核凭据。生产 crate 全局禁止 `unsafe`，不直接调用 `libc`；如果目标平台所需调用无法由当前 `nix` 安全接口覆盖，应停止该平台实现并重新评审依赖方案，不能在 adapter 中临时放宽这一约束。

MVP 不使用 control bearer token、随机 socket 文件名、keyring 或挑战响应。

## 同 UID 自批准边界

socket 文件权限和 peer UID 无法区分同 UID 的宿主 CLI 与 sandboxed Agent。MVP 不强制用户采用项目生成的 profile，也不接管 Agent 启动；同 UID 防自批准最终由用户的 nono profile 与启动方式负责。

Linux 若需要隔离 pathname Unix socket，应启用：

```json
{"linux":{"af_unix_mediation":"pathname"}}
```

并避免给 control socket、其父目录或覆盖它的 subtree 添加 `filesystem.unix_socket*` grant。macOS 应使用能限制 Unix socket 的 Blocked 或 ProxyOnly restricted network mode，而不是 AllowAll。

## Profile Validation

```bash
nono-approval config validate --profile <name-or-path>
```

这是显式诊断命令，不是启动门禁。它通过已安装的 nono 和用户指定的最终 profile 启动一个短生命周期真实沙箱，并在其中运行隐藏的 `__probe-control-socket`。

探针只连接 control socket 并调用无状态的：

```text
GET /v1/status
```

它不创建 Approval Request，也不调用 approve/deny。

只有父进程确认探针已在沙箱内启动，并且 `connect(2)` 明确收到 `EACCES` 或 `EPERM` 时返回成功。以下情况全部返回非零：

- control socket 可达或收到任何 HTTP 响应；
- daemon 未运行；
- nono 不可用或 profile 无效；
- sandbox 初始化失败；
- 探针未启动或被 command policy 拒绝；
- 输出协议无效、超时或其他连接错误。

探针使用短超时和固定版本的父子协议，至少报告 `started` 与 `denied(errno)`。不得把 ENOENT、sandbox 未启动或不确定状态误报为安全。

普通 `nono run --profile ...` 会在宿主侧执行最终 profile 的 `session_hooks.before/after`，因此 validation 可能额外执行一次用户 hook，CLI 必须明确提示。MVP 不复制或重写 profile 来移除 hooks。

Validation 不安装 after hook，也不把 hook 纳入安全保证：after 时机太晚、在宿主执行、可被 child profile 覆盖，而且同 profile 自调用可能递归。结果只证明当前 nono 版本、当前解析后的 profile 和当前 control socket 下的本次行为，不证明用户之后用相同配置启动 Agent。

## 请求洪泛与重放

- webhook body 默认 `256 KiB`，global pending 默认 `64`，per-session pending 默认 `8`，输出也有硬上限；
- body 超限时在解析前返回 `413`，内容不进入 pending、日志或 Debug Capture；
- per-session 满返回 `429`，全局满返回 `503`；容量拒绝不驱逐已有请求，也不进入 Debug Capture；
- 重复 `(session_id, request_id)` 拒绝；
- completion 后保留短期 replay cache；
- webhook caller 永远不能访问 control API；
- 非 loopback bind 默认拒绝。

## 配置解析

`config.toml` 必须声明 `schema_version = 1`，并以拒绝未知字段的方式解析。安全相关字段拼错、版本缺失或 schema 不受支持时，`setup` 与 `serve` 都返回非零；不能用默认值悄悄替代无效的显式配置。`serve` 不迁移或改写配置。

运行参数遵循 `CLI > config.toml > 内置默认值`，且每一层都执行相同验证。固定 webhook path 不进入配置，也没有覆盖入口。

## 终端注入防护

以下内容全部不可信：command、args、path、host、URL、reason、caller、rule label 和 session ID。

所有 CLI/TUI 输出必须：

- 删除 ANSI escape sequence；
- 替换 C0/C1 control characters；
- 对换行、制表符和不可打印字符使用可见转义；
- 安全转义后的完整决策详情总量限制为 `1 MiB`；
- 禁止改变终端标题、输出 hyperlink 或控制光标；
- 使用确定性 quoting，仅供展示，绝不重新交给 shell 执行。

详情超限时必须在 ingress 阶段拒绝整个请求，不能通过截断、折叠或省略后缀让用户基于不完整内容决定。列表摘要可以明确截断，因为它只用于导航；`show` 和 TUI 详情必须完整、自动换行并支持纵向滚动。

用户输入的 denial reason 也是不可信边界输入：必须是非空 UTF-8，编码后最多 `4 KiB`，进入终端输出和 Debug Capture 时执行相同的安全转义规则。验证失败不能截断后继续提交。

## 明文展示边界

MVP 不自动脱敏 token、密码、签名 URL 或用户内容。审批人需要看到 nono 实际请求批准的完整已知操作，启发式脱敏可能隐藏关键差异。

正常模式边界：

- pending request 的完整已知字段可以通过 owner-only control 面明文返回；
- 普通视图只展示操作与必要规则上下文，不塞入无助于决定的技术 metadata；
- 明文始终经过终端安全转义；
- raw JSON 和未知附加字段不进入普通或调试视图；
- 请求详情不写入普通日志或磁盘；
- terminal state 后立即销毁详情，仅保留 Tombstone。

## 来源模型

合法 webhook 中各字段来源并不统一，loopback HTTP 也不能证明 caller 身份。provenance 只用于解析、测试和调试，不能成为额外授权依据。

内部只保留用于选择展示模板的 `SourceKind`：

```text
tool_sandbox
proxy
capability
network
```

普通 UI 不展示通用可信度标签。值为 `proxy` 的 session ID 和值为 `0` 的 child PID 在普通视图中视为缺失；Debug Capture 保留 wire 原值。字段来源与缺失信息详见 [nono 0.69 调研](../research/nono-0.69.md)。

## 普通日志

默认日志只记录：

- approval ID；
- capability type；
- session ID 的短形式；
- 状态转换；
- 等待时长；
- 错误类别。

默认不记录完整 args、path、URL、raw JSON 或 denial reason。nono 自己承担真正的安全审计；daemon 日志只用于运行诊断。

## Debug Capture

显式启用：

```text
--debug-capture
```

daemon 不接受任意捕获路径。每次启用时，在项目托管的 owner-only state directory 中创建一个新的 `0600` NDJSON 文件；目录必须为当前 UID 所有、权限 `0700` 且路径组件不含 symlink，否则启动失败。启动 banner 和 `status` 必须持续显示 Debug Capture 已启用及当前文件位置。普通 text/JSON 日志策略不因 Debug Capture 改变。

### 格式

使用 UTF-8 JSON Lines（NDJSON）追加写入。每条记录包含整数 `schema_version`，先完整序列化成不含物理换行的单行，再追加写入；不维护或重写 JSON 数组。

进程崩溃时，此前完整行可独立解析，读取方忽略末尾唯一可能存在的不完整行。

### 记录类型

只写两类事件：

- `request_received`：完整已知 Wire DTO、既有来源信息和 daemon 本地 deadline；
- `request_completed`：approval ID、终态、决定来源、可选拒绝理由、等待时长和 webhook response delivery outcome。

completion 记录不重复 Wire DTO，通过 approval ID 与 received 记录关联。control API 轮询、list/show、TUI 选择与滚动、倒计时重绘不进入捕获文件。

### 来源字段

Debug Capture 复用既有来源模型，不建立第二套 provenance schema：

- 外层 `backend` 记录为 `claimed_backend`；
- 保留 Wire DTO 中实际存在的 request ID、session ID、caller、规则字段、reason、child PID；
- 记录由 variant 推导的 `source_kind`；
- 保留 wire 中的精确已知值。

不虚构或额外记录 webhook 未提供且 daemon 无法可靠确认的 resolved executable identity、cwd、profile identity/digest、supervisor PID、session display name、Agent entrypoint、统一 observed child PID 和 nono resolved deadline。也不记录全部 HTTP headers、loopback peer address、平台标签或 control peer 信息。

### 文件保留与清理

捕获文件不自动轮换、不自动过期、不在 daemon 启动或退出时删除。每次启用 capture 的 daemon 启动都创建新文件，历史文件由用户负责保留。

`nono-approval debug captures` 只列出合法托管文件的名称、创建时间和大小，不读取明文内容。`nono-approval debug clean` 显式删除全部合法托管文件且不二次确认。清理必须逐项验证当前 UID owner、regular file 类型和托管命名规则；拒绝 symlink、子目录及异常条目，不递归删除，也不删除托管目录。任何异常或删除失败都使命令返回非零。

### 运行时失败

`--debug-capture` 启动阶段无法安全创建目录或文件时，daemon 启动失败，不能静默忽略用户显式要求的捕获。

daemon 运行期间首次追加失败时，立即关闭本次 Debug Capture 并进入不可恢复的 `failed` 状态，但继续处理已有和后续审批；Debug Capture 不是审批安全边界，磁盘写满或诊断文件故障不能阻塞人工决定。实现只记录一次醒目错误，不反复重试、不切换新文件，也不持续刷相同日志。`status` 和 TUI 底部状态区持续显示 `debug capture: failed` 及非敏感错误类别，直到 daemon 重启。
