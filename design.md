# nono 本地审批守护进程

> Draft。本文是 MVP 设计入口，只保留产品边界、总架构、关键决策和文档导航。协议、安全、CLI/TUI、运维与测试细节分别由专题文档维护，不在此复制。

## 背景

nono 已经实现 `allow`、`approve`、`deny` 规则、Approval Request 和暂停等待决定的语义，但 terminal backend 在 Pi、Claude Code、Codex 等全屏 Agent TUI 中会与 Agent 争用同一个 TTY。真实验证中，审批输入被 Pi 消费，最终请求超时并 fail closed。

nono-approval 通过现有同步 webhook ApprovalBackend，把人工交互移到另一个终端：

```text
Agent under nono
      │ approvable request
      ▼
nono webhook backend
      │ loopback HTTP, synchronous wait
      ▼
nono-approval serve
      │ owner-only Unix control socket
      ▼
nono-approval interactive TUI / exact CLI commands
```

本项目不代理 sudo、密码或命令执行，只响应 nono 已经判定为可进入审批流程的请求。

## MVP 体验

一次性初始化：

```bash
nono-approval setup
```

前台运行 daemon：

```bash
nono-approval serve
```

在另一个终端直接打开交互式审批：

```bash
nono-approval
```

TUI 持续等待 pending request，展示完整已知操作，用户按 `a` 或 `d` 立即批准或拒绝，不需要复制 ID，也没有二次确认。脚本和高级用户仍可使用 `status/list/show/approve/deny`。

## 目标

- 不修改 nono 即兼容当前 webhook ApprovalBackend；
- Agent TUI 不需要退出、暂停或让出终端；
- 交互式查看并决定一个精确 pending request；
- 支持 command、endpoint、capability 和 network wire variant；
- daemon、control 或 webhook 错误时 fail closed；
- pending detail 默认只在内存中存在，完成后立即销毁；
- owner-only Unix socket 承载 control，不开放 TCP 管理面；
- webhook 默认只监听固定 loopback 地址；
- 请求体、pending 数量、等待时间和输出都有明确上限；
- 请求明文展示但始终终端安全转义，正常模式不落盘、不写详情日志；
- Linux 与 macOS 都有明确的 runtime path 和 peer identity 实现；
- 内部 control 协议可供未来桌面 UI 或通知客户端复用。

## 非目标

- 修改 nono 源码或削弱其 hard deny；
- 自动判断操作是否安全或自动批准；
- session 级、永久允许或自动生成/修改 profile；
- 恢复 daemon 重启前的 pending request；
- 作为权威审计日志；
- 在同 UID 已能读取进程内存或控制用户会话时提供额外保护；
- 跨主机审批、浏览器 UI、Slack bot 或移动端；
- 自动安装、启用或修改 systemd/launchd 服务；
- 成为通用策略引擎；
- 解决 nono 各平台 sandbox 的能力差异。

## 总架构

```text
┌────────────────── nono ──────────────────┐
│ Tool Sandbox / proxy / supervisor        │
│ ApprovalBackend::request_approval        │
└───────────────────┬──────────────────────┘
                    │ loopback webhook
                    ▼
┌────────────── nono-approval serve ──────────────┐
│ Wire Adapter -> Broker/PendingStore             │
│        ├── Approval Lease / state machine       │
│        ├── Debug Capture (explicit only)        │
│        └── HTTP over owner-only Unix socket     │
└───────────────────┬─────────────────────────────┘
                    ▼
        interactive TUI / exact CLI clients
```

生产二进制不依赖 nono crate。nono 只作为 wire 事实来源与兼容性测试依赖。

## 已确认的核心决策

### 接入与配置

- 固定 loopback endpoint `http://127.0.0.1:17443/v1/webhooks/approval`；
- `setup` 显式、幂等，`serve` 不隐式初始化；
- 配置使用 `$XDG_CONFIG_HOME/nono-approval/config.toml`，必须声明 `schema_version = 1`，未知字段报错；
- 运行参数按 `CLI > config.toml > 内置默认值` 解析；
- webhook ingress 与 Unix control socket 分离；
- nono `request_id` 不作为 control 主键，每次 ingress 生成独立完整 approval ID。

### 生命周期

- daemon 单调时钟 deadline 是唯一 Approval Lease；
- daemon 默认 Approval Lease 为 `270s`，`setup` 输出的 nono backend/default timeout 为 `300s`；
- webhook request body 默认上限为 `256 KiB`，超限在解析前返回 `413`；
- pending 默认全局上限 `64`、每 session 上限 `8`，容量满时不驱逐已有请求；
- approval ID 使用 `appr_` 加 16 位小写十六进制随机数，碰撞时重新生成；
- webhook disconnect 只做 best-effort 早期清理；
- 每个请求只能从 Pending 进入一个 terminal state；
- 完成后销毁详情，仅保留 10 分钟/1024 条最小 Tombstone；
- 决定只作用于当前精确请求一次。

### Control 安全

- runtime directory `0700`、socket `0600`，每条连接验证 peer UID；
- 不使用 bearer token、随机 socket path、keyring 或 challenge-response；
- 同 UID 防自批准由用户最终 nono profile 与启动方式负责；
- 提供真实短生命周期 sandbox 的 `config validate`，但不作为强制启动门禁；
- 只有确认 probe 已启动且 connect 收到 `EACCES/EPERM` 才报告安全。

### 用户交互

- 不带子命令运行时进入 `ratatui + crossterm` 全屏 TUI；
- 空队列保持等待，每 `500ms` 轮询 control API；
- TUI 启动时 daemon 不存在则保持打开并每 `1s` 重连，连接成功后自动进入正常轮询；
- 已连接 daemon 断开时立即清除旧请求快照并进入每 `1s` 重连的等待状态；
- pending queue 按 `received_at` 最早优先，新请求追加且不抢占当前选择；
- 正常尺寸左右双栏，窄终端切单栏并用 `Tab` 切换；
- `a/d` 立即决定；Enter 永不批准，只在明确的拒绝理由输入态提交 denial；
- 另提供“填写理由后拒绝”的快捷入口，不牺牲 `d` 的单键快速拒绝；
- 自定义拒绝理由按 UTF-8 编码后最多 `4 KiB`，不允许空值或静默截断；
- `list` 摘要可按宽度截断，决策详情自动换行且不得省略；安全转义后的详情上限为 `1 MiB`；
- `j/k` 和方向键只移动请求选择，详情使用独立滚动键；
- `show/approve/deny` 只接受完整 approval ID，不支持唯一前缀。

### 数据与调试

- pending 请求在 owner-only control 面明文展示，不做自动脱敏；
- 所有终端输出仍清理 ANSI/OSC/control characters；
- 正常模式不持久化详情，不把详情写入普通日志；
- 显式 Debug Capture 在项目托管的 owner-only state directory 中为每次 daemon 启动创建 NDJSON；
- Debug Capture 只记录 `request_received` 与 `request_completed`；
- 不自动轮换或过期，`debug clean` 显式清除全部托管捕获文件；
- 来源字段复用同一套 nono wire provenance 模型，不虚构缺失 metadata。

### 交付

- MVP 只直接提供前台 `serve`；
- 仓库提供 systemd user service 与 launchd agent 示例，不自动配置；
- MIT License；
- crates.io 与 GitHub Releases。
- GitHub Releases 提供 Linux/macOS 的 x86_64 与 aarch64 GNU/Darwin 归档及 `SHA256SUMS`；
- MVP 不发布 musl、deb/rpm、Homebrew tap 或安装脚本。

### 平台目录

- 使用 `directories::ProjectDirs` 解析平台原生 config、state/cache 与 runtime 基础目录；
- 产品设计只规定目录用途、owner-only 权限和 socket 路径长度约束，不硬编码 macOS 绝对路径；
- 平台适配测试固定当前依赖版本在 Linux/macOS 的实际解析结果。

## 文档地图

- [领域语言](CONTEXT.md)：Approval Lease、Tombstone、Wire Adapter 等术语；
- [协议与适配边界](docs/design/protocol.md)：webhook schema、Wire Adapter、control API；
- [审批生命周期](docs/design/approval-lifecycle.md)：broker、状态机、deadline、Tombstone、并发；
- [安全模型](docs/design/security.md)：信任边界、socket、Profile Validation、明文边界、Debug Capture；
- [CLI 与 TUI](docs/design/cli-and-tui.md)：命令、展示、轮询、布局和键位；
- [运行与发布](docs/design/operations.md)：runtime path、profile 示例、服务示例、依赖和发布；
- [实现与验证](docs/design/testing.md)：实现阶段、测试矩阵与 MVP 验收；
- [nono 0.69 调研](docs/research/nono-0.69.md)：版本绑定的源码事实、限制和上游问题；
- [ADR 0001](docs/adr/0001-daemon-deadline-defines-approval-lease.md)：daemon deadline 定义 Approval Lease；
- [ADR 0002](docs/adr/0002-support-linux-and-macos-through-local-platform-adapters.md)：Linux/macOS 平台适配边界；
- [ADR 0003](docs/adr/0003-leave-same-uid-control-isolation-to-deployment.md)：同 UID 隔离交给部署配置。

## 已知限制

- webhook 是同步长连接，每个 pending request 占用轻量 task 与连接；
- request body 不含 nono 最终 resolved deadline；
- nono 当前没有可靠 cancellation message；
- loopback ingress 没有 peer UID；
- webhook caller 不认证，任意本地进程都可提交伪造请求或占用容量，但不能借此执行操作或决定其他请求；
- 明文审批内容可能包含秘密；
- Profile Validation 只证明本次最终 profile 的真实行为，不保证用户随后使用同一配置。

详细原因与证据见 [审批生命周期](docs/design/approval-lifecycle.md)、[安全模型](docs/design/security.md) 和 [nono 0.69 调研](docs/research/nono-0.69.md)。

## 设计状态

当前 grill 中发现的 MVP 产品、安全、交互、配置和发布决策均已收敛。实现阶段若暴露新的跨模块取舍，应先补入对应专题文档；局部实现细节按既有约束选择最简单方案。

完整验收清单见 [实现与验证计划](docs/design/testing.md#mvp-验收)。
