# CLI 与交互式审批界面

本文是 setup、serve、控制命令、请求展示和全屏 TUI 的唯一事实来源。协议语义见 [协议与适配边界](protocol.md)，权限边界见 [安全模型](security.md)。

## 命令总览

```text
nono-approval                         # 交互式审批界面
nono-approval setup
nono-approval config validate --profile <name-or-path>
nono-approval serve [OPTIONS]
nono-approval status
nono-approval list [--json]
nono-approval show <approval-id> [--debug]
nono-approval approve <approval-id>
nono-approval deny <approval-id> [--reason <text>]
nono-approval debug captures
nono-approval debug clean
```

不带子命令时直接进入交互式界面。MVP 不增加语义重复的 `interactive` 或 `review` 子命令。

## `setup`

`setup` 是唯一创建初始配置的入口：

1. 创建 `$XDG_CONFIG_HOME/nono-approval/`；
2. 原子写入权限为 `0600` 的 `config.toml`，并声明 `schema_version = 1`；
3. 输出固定 Webhook Endpoint；
4. 输出可合入任意 nono profile 的 approval backend JSON 片段；
5. 提示运行 Profile Validation 检查 control socket 隔离。

`setup` 幂等：配置已存在时只检查权限、schema 与内容并重新输出集成信息，不创建或修改 nono profile。

配置缺少 `schema_version`、版本不是 `1`、包含未知字段或 TOML 无法解析时，`setup` 返回非零且不覆盖原文件。它只能从零创建当前 schema，不负责自动迁移用户已有配置。

## `config validate`

```bash
nono-approval config validate --profile <name-or-path>
```

这是非强制的真实 sandbox 探针，详细语义见 [安全模型](security.md#profile-validation)。CLI 必须明确提示目标 profile 的宿主级 session hooks 可能被额外执行一次。

## `serve`

```text
nono-approval serve [OPTIONS]

Options:
  --webhook-listen ADDR       loopback webhook listener
  --control-socket PATH       Unix control socket
  --request-timeout DURATION  pending request timeout [default: 270s]
  --max-pending COUNT         global pending limit [default: 64]
  --max-per-session COUNT     per-session pending limit [default: 8]
  --max-body SIZE             webhook request body limit [default: 256KiB]
  --debug-capture             write plaintext diagnostic events to managed NDJSON
  --log-format text|json      log output format
```

`serve` 只读取既有配置，不生成 token，也不隐式初始化。配置缺失、权限不安全、schema 不受支持或包含未知字段时启动失败；它不迁移、不修复也不改写配置文件。

运行参数使用确定的覆盖顺序：

```text
显式 CLI 参数 > config.toml > 内置默认值
```

`serve` 暴露的运行参数可以临时覆盖，但仍执行相同的类型、范围和安全验证。Webhook Endpoint 使用固定 path，listener 地址被覆盖时 endpoint 的 host/port 随之变化，用户必须同步更新 nono profile。

`setup` 输出的 nono profile 片段默认把 approval backend 和 approval defaults timeout 都设为 `300s`。daemon 的 `270s` 是唯一 Approval Lease；额外 30 秒只用于尽力交付明确 denial。用户覆盖任一侧后，CLI 只能提示应保持 nono timeout 更长，无法从当前 webhook 确认最终 resolved timeout。

启动时打印：

```text
nono-approval is ready
  webhook: http://127.0.0.1:17443/v1/webhooks/approval
  control: /run/user/<uid>/nono-approval/control.sock
```

每次以 `--debug-capture` 启动时，在项目托管的 state directory 中创建一个新的 owner-only NDJSON 文件，不接受任意输出路径。Debug Capture 的安全与格式要求见 [安全模型](security.md#debug-capture)。

## `status`

```text
Daemon: running
Pending: 2
Started: 8m 31s ago
Webhook: 127.0.0.1:17443
Debug capture: disabled
```

Debug Capture 运行中写入失败后，`status` 显示 `Debug capture: failed`；交互式界面底部状态区持续显示相同故障。该状态不会自动恢复，必须修复外部原因并重启 daemon。审批功能继续可用。

## `list`

```text
ID             TYPE      REQUEST                         AGE   EXPIRES
appr_7d8f2c6a1b3e4f50  command   gh repo create demo --private   4s    4m26s
appr_a91b04dd5e6f7081  endpoint  POST /repos/acme/demo/issues    1s    4m29s
```

支持 `--json`，只列 pending request。

`list` 的人类可读 summary 只用于队列导航，可以按当前终端列宽使用明确的省略标记截断；`--json` 不按终端宽度截断。

## `show`

```text
Approval: appr_7d8f2c6a1b3e4f50
Command: gh repo create demo --private
Requested by: Tool Sandbox
Caller: session
Rule: invocation_policy.approve[0]
Reason: repository creation requires user approval
Received: 2026-07-27 20:00:00 +08:00
Expires: in 4m 26s
```

普通视图不展示 claimed backend、nono request ID、session ID 或 child PID。显式 `--debug` 把完整已知 Wire DTO 与来源字段放在独立 Debug 区域，不能挤占主决策内容。

`show` 和 TUI 的决策详情不得截断字段或隐藏后缀。长 command、argument、path、URL 和 reason 按当前内容区域宽度自动换行；换行后超过可见高度时使用既有纵向滚动键浏览。客户端不提供横向滚动。

## `approve` 与 `deny`

```bash
nono-approval approve appr_7d8f2c6a1b3e4f50
nono-approval deny appr_7d8f2c6a1b3e4f50
nono-approval deny appr_7d8f2c6a1b3e4f50 --reason "outside this task"
```

命令本身就是最终决定，不询问 `Are you sure?`。

MVP 不支持：

```text
approve --latest
approve --all
approve gh
approve 7d8f
```

`show/approve/deny` 必须接收 `appr_` 加 16 位小写十六进制字符的完整 approval ID，按完整字符串精确匹配。缩写、大写、其他编码、未知 ID 或已结束请求直接失败，不能按队列猜测目标。

## `debug captures` 与 `debug clean`

```bash
nono-approval debug captures
nono-approval debug clean
```

`debug captures` 列出项目托管目录内的捕获文件、创建时间和大小，不读取或打印其中的明文审批内容。

`debug clean` 删除托管目录内全部合法捕获文件，命令本身就是最终确认，不二次询问。它只处理当前 UID 拥有的 regular file，拒绝 symlink、子目录、异常 owner 或不符合托管命名规则的条目；不递归删除，也不删除托管目录本身。遇到异常条目时整体返回非零，并保留异常条目。

## 请求展示模板

### Command

- command；
- args；
- caller；
- intercept rule；
- reason。

参数使用确定性 shell quoting，仅用于显示，不重新交给 shell 执行。

### Endpoint

- route ID；
- HTTP method；
- upstream；
- path；
- rule label；
- reason。

### Capability

- path；
- access mode；
- reason。

### Network

- host；
- port；
- protocol；
- resolved IPs；
- reason。

普通用户视图只展示操作本身和必要规则上下文。技术来源字段与可信度差异不进入主视图，详见 [安全模型](security.md#来源模型)。

## 交互式入口

```bash
nono-approval
```

交互式界面是 MVP 的主要用户路径。它在同一终端中展示 pending queue、当前请求的完整明文详情和剩余 Approval Lease，并直接批准或拒绝，不要求复制 ID。

TUI 始终通过 owner-only control API 工作，不直接访问 webhook handler 或 broker 内部状态。退出 TUI 不影响 daemon 或 pending request。

## 刷新与空队列

- TUI 启动时 daemon 不存在或 control socket 尚未就绪，不退出，显示 `Waiting for daemon…`；
- 等待状态每 `1s` 尝试连接一次，不采用指数退避，也不输出重复错误日志；
- 首次连接成功后自动切换到正常审批界面和 `500ms` 轮询，不要求重启 TUI；
- 没有 pending request 时保持打开并显示等待状态；
- 每 `500ms` 轮询一次 control API；
- 新请求自动出现；
- pending queue 按 `received_at` 升序排列，最早请求在前；
- 新请求只追加到队列末尾，不能抢占当前选择或重置详情滚动位置；
- 倒计时根据响应 deadline 在客户端本地平滑更新；
- 轮询和本地倒计时不得延长 daemon Approval Lease；
- MVP 不增加 event stream、广播队列或独立断线重连协议。

用户仍可在等待 daemon 时按 `q` 正常退出。等待状态不得自动启动 `serve`、隐式执行 `setup` 或创建 control socket。

TUI 已连接后，如果 control 请求发现 daemon 退出、control socket 被替换或连接不可用，立即进入 `Disconnected — waiting for daemon…` 状态并恢复每 `1s` 重连。断线瞬间必须清除旧 pending snapshot、详情、当前选择、滚动位置和未提交的 denial reason；旧请求不能继续显示为可操作，也不能在重连后恢复。新 daemon 建立连接后，只使用它当前返回的状态重新初始化界面。

TUI 不依赖 approval ID 判断 daemon 是否还是同一实例。即使重启后极低概率出现相同 ID，旧客户端状态也已经在断线时销毁，不能跨 daemon 生命周期复用。

## TUI 技术与布局

使用 `ratatui + crossterm`。

正常尺寸采用左右双栏：

- 左：稳定宽度的 pending queue；
- 右：选中请求的完整明文详情；
- 底：固定高度的连接状态、错误反馈和操作提示。

队列更新、倒计时变化、选择与详情滚动不能导致整体布局跳动。

详情文本按右栏或单栏的实际内容宽度自动换行。resize 后重新计算换行，但按逻辑内容位置尽可能保持阅读位置；自动换行不得插入、删除或截断原始已知字段，只改变屏幕排版。

当前选中请求完成、过期或从下一次轮询结果中消失时，按它在旧队列中的位置选择新队列里的下一项；原位置之后没有项目时才选择上一项；队列为空则进入等待状态。选择迁移必须按完整 approval ID 和旧索引计算，不能因为排序刷新跳到不相关请求。

终端宽度不足时自动切为单栏，不水平压缩两栏。单栏默认显示详情，`Tab` 在队列和详情之间切换。双栏/单栏切换必须按完整 approval ID 保留选择，并尽可能保留滚动位置，不能因 resize 改变决定目标。具体宽度阈值由组件最小宽度在实现中固定，并覆盖阈值边界测试。

## 键位

```text
j / Down       下一个请求
k / Up         上一个请求
a              立即批准选中请求
d              立即拒绝选中请求
D              打开理由输入态，填写后拒绝
q              退出 TUI
Tab            单栏时切换队列/详情
Ctrl-d/u       详情向下/向上半页
PageDown/Up    详情向下/向上整页
g / G          详情顶部/底部
```

约束：

- `a/d` 不增加二次确认；
- `d` 使用固定 reason `denied by local user` 立即拒绝；
- `D`（Shift-d）打开单行理由输入态，允许用户编辑明文拒绝理由后提交 denial；
- 普通浏览态的 Enter 不绑定批准，也不触发任何审批决定；
- 方向键和 `j/k` 始终只移动请求选择，不随焦点或布局改变含义；
- 详情滚动键只改变详情视口；
- 决定时携带按键瞬间选中项的完整 approval ID；
- 请求已过期、消失或被其他客户端决定时，只在底部状态区显示失败，不得把同一次按键应用到更新后的其他请求。

理由输入态不改变当前选中的完整 approval ID；轮询仍继续，若目标在编辑期间结束，提交时只报告失败。界面必须明确显示 `Deny reason:` 输入提示，此时 Enter 提交 denial，Esc 取消并丢弃当前输入。Enter 在任何状态下都不能批准请求。

自定义 reason 必须非空，按 UTF-8 编码后最多 `4 KiB`。空输入时 Enter 不提交并在输入区显示提示；达到上限后拒绝继续输入。TUI、CLI `--reason` 和 control API 使用同一验证规则，超限直接报错，不能静默截断。用户不想填写理由时应按 Esc 退出输入态，再用 `d` 以固定理由快速拒绝。

## 终端恢复

TUI 必须在正常退出、错误返回、panic hook 和信号处理路径中恢复 raw mode、alternate screen、光标与终端状态。恢复失败需要写入 stderr，但不能以再次 panic 的方式掩盖原始错误。
