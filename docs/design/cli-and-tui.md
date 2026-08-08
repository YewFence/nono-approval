# CLI 与 TUI

本文描述当前 `src/cli.rs` 和 `src/interactive.rs` 的用户界面。control JSON 和状态码
见[协议与适配](protocol.md)，输入安全约束见[安全模型](security.md)。

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
nono-approval completions <bash|elvish|fish|powershell|zsh>
```

不带子命令时直接进入 TUI。`__probe-control-socket` 是供 Profile Validation 调用的
隐藏子命令，不作为用户 interface 承诺。

所有 approval ID 在 clap 解析阶段就必须是完整的 `appr_` 加 16 位小写十六进制字符。
`show` 支持 pending detail 和保留期内的 completed Tombstone；`approve`/`deny` 只接受
仍处于 pending 的请求。

## `setup`

`setup` 解析 `ProjectDirs` 的平台原生 config path，创建或验证：

```text
<ProjectDirs.config_dir()>/config.toml
```

配置文件由当前用户拥有、权限 `0600`，并使用 `schema_version = 1`。首次创建使用原子
临时文件；已有文件则只执行安全加载和 schema/值验证，不覆盖、不迁移。

成功输出：

1. 配置文件路径；
2. 当前 `webhook.listen` 生成的完整 endpoint；
3. `local-broker` webhook backend 和 `approval_defaults` 的 nono JSON 片段，timeout
   为 `300s`；
4. Profile Validation 命令提示。

`setup` 不启动 daemon，不修改 nono profile，不创建 control socket。

## `config validate`

```bash
nono-approval config validate --profile <name-or-path>
```

命令在启动真实 probe 前向 stderr 提示目标 profile 的宿主侧 session hooks 可能执行。
probe 必须先输出 `nono-approval-probe-v1 started`，然后在 sandbox 内连接 control
socket；只有 errno `1`（`EPERM`）或 `13`（`EACCES`）才成功。可达、未启动、协议错误、
其他 errno 和 15 秒超时都返回非零。

该命令可用隐藏的 `--control-socket` 指定路径，但不会创建 approval request，也不会
调用 decision API。

## `serve`

```text
nono-approval serve [OPTIONS]

Options:
  --webhook-listen ADDR       loopback webhook listener
  --control-socket PATH       override platform control socket
  --request-timeout DURATION  Approval Lease duration
  --max-pending COUNT         global pending limit
  --max-per-session COUNT     per-session pending limit
  --max-body SIZE             webhook request body limit
  --debug-capture             write managed NDJSON for this daemon run
  --log-format text|json      text or structured JSON logs
```

`serve` 先加载平台 config，再按以下顺序合并运行值：

```text
显式 CLI 参数 > config.toml > 内置默认值
```

CLI override 仍检查 loopback、正数限制以及 `max_per_session <= max_pending`。缺少配置、
不安全权限、未知字段、非法 schema 或无效值都会启动失败；`serve` 不隐式执行 `setup`。

默认值来自实现：Lease `270s`、global `64`、per-session `8`、body `256KiB`、detail
`1MiB`。Webhook path 固定，但 listener host/port 可以显式覆盖；覆盖后必须同步修改
nono profile。

daemon 启动成功后打印 webhook endpoint 和实际 control socket。启用 Debug Capture 时
还打印本次 capture 文件路径。

## `status`

示例：

```text
Daemon: running
Pending: 2
Started: 8s ago
Webhook: 127.0.0.1:17443
Debug capture: enabled (/.../debug-captures/2026-...ndjson)
```

Debug Capture 可能显示 `disabled`、`enabled (path)` 或 `failed (category)`。失败状态
不会自动恢复，但不会阻塞审批。

## `list`

默认人类输出只有三个字段：

```text
ID                     TYPE        REQUEST
appr_7d8f2c6a1b3e4f50   command     date
```

`--json` 输出 control API 的完整 `ApprovalList`。人类输出的 summary 按当前终端宽度
使用 `…` 截断；API 和 JSON 输出不按终端宽度截断。列表只包含 pending request，不包含
Tombstone。

## `show`

```bash
nono-approval show appr_7d8f2c6a1b3e4f50
nono-approval show --debug appr_7d8f2c6a1b3e4f50
```

pending 请求以字段逐行输出：

```text
Approval: appr_7d8f2c6a1b3e4f50
Command: date
Requested by: Tool Sandbox
Caller: session
Rule: <catch-all>
Reason: ...
Received: 2026-07-27T12:00:00Z
Deadline: 2026-07-27T12:04:30Z
```

`--debug` 额外输出 claimed backend、source kind 和已知 Wire DTO。已结束但 Tombstone
仍保留时只输出 approval ID、终态和完成时间；详情不会恢复。未知或已过期 Tombstone
返回错误。

字段值先经过终端安全清理。CLI 不自行做语义截断或横向滚动；长行由终端正常换行，
只有 `list` summary 使用显式省略号。

## `approve` 与 `deny`

```bash
nono-approval approve appr_7d8f2c6a1b3e4f50
nono-approval deny appr_7d8f2c6a1b3e4f50
nono-approval deny appr_7d8f2c6a1b3e4f50 --reason "outside this task"
```

命令本身就是最终决定，不二次确认。无 `--reason` 的 `deny` 使用固定理由
`denied by local user`。reason 必须通过 Broker 的统一验证：非空、不全为 NUL、UTF-8
编码后不超过 `4 KiB`。

`approve` 和 `deny` 不支持 `--latest`、`--all`、操作名或 ID 前缀。未知、已结束、已过期
或被其他客户端决定的请求失败，绝不回退到其他队列项。

## Debug Capture 命令

```bash
nono-approval debug captures
nono-approval debug clean
```

`debug captures` 只列出托管文件的名称、创建时间和字节数，不读取内容。
`debug clean` 删除全部通过 owner、regular file、`0600` 和固定命名规则校验的文件，不
递归删除目录；遇到不安全条目时返回非零。

## Shell completion

```bash
nono-approval completions bash
nono-approval completions elvish
nono-approval completions fish
nono-approval completions powershell
nono-approval completions zsh
```

## TUI 入口与刷新

```bash
nono-approval
```

TUI 只通过 control client 工作。初始状态为 `Disconnected — waiting for daemon…`，
control socket 不可用时每 `1s` 重连；连接成功后每 `500ms` 获取 list、status 和当前
detail。没有 pending 时保持打开并显示等待状态，不自动运行 `setup` 或启动 daemon。

断线会立即清除 approvals、选择、详情、详情滚动和未提交的 denial reason。重连后只使用
新 daemon 当前返回的数据，不能跨 daemon 生命周期复用旧快照。

刷新时优先按完整 approval ID 保留当前选择；目标消失后退回旧索引仍在范围内的项目。
没有旧选择时选择第一项。详情请求遇到 completed 或 not-found 会清空详情，不会把决定
转移到其他项目。

## TUI 布局与键位

宽度至少 `90` 列时使用 38%/62% 左右双栏；更窄时显示单栏，`Tab` 切换队列和详情，
默认显示队列。详情使用 ratatui `Wrap` 自动换行和纵向 scroll；不会水平滚动或截断
字段。双栏/单栏切换不改变选中的 approval ID。

```text
j / Down       下一个请求
k / Up         上一个请求
a              立即批准
d              用固定理由立即拒绝
D              打开理由输入态
q              退出 TUI
Ctrl-c         退出 TUI
Tab            窄屏切换队列/详情
Ctrl-d/u       详情下/上滚动
PageDown/Up    详情下/上滚动
g / G          详情顶部/底部
```

浏览态 Enter 没有任何审批含义。理由输入态 Enter 提交、Esc 丢弃；输入期间目标
approval ID 固定，目标若在下一次刷新中结束，提交只显示失败。`D` 和 CLI `--reason`
都使用 Broker 的同一套验证规则。

TUI 在正常返回和 panic hook 中调用 ratatui restore，恢复 alternate screen、raw mode、
光标和终端状态。
