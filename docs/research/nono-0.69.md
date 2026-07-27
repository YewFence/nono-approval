# nono 0.69 Webhook 与 Sandbox 调研

本文记录设计所依赖的版本化外部事实，不作为 nono-approval 自身长期协议承诺。基线版本：`nono 0.69.0`，调查时间：2026-07-27 至 2026-07-28。

## Approval Backend

nono 已有多种 ApprovalBackend：

- terminal：从 `/dev/tty` 读取交互输入；
- webhook：同步 HTTP 请求并等待响应；
- exec：调用外部程序；
- chain：以 all/any 组合 backend。

本项目使用现有 webhook，不修改 nono。

## TUI 输入冲突

Pi、Claude Code、Codex 等全屏 Agent TUI 占用当前终端时，terminal backend 与 Agent 会争用同一个 TTY。2026-07-27 本地验证中，Pi 触发 `date` 的 approve 规则后，nono 显示审批提示，但两次 `y` 都进入 Pi PTY，最终审批超时并返回 `approval_denied`。

因此本地审批交互必须移到独立终端，而不是继续争抢 Agent 的 `/dev/tty`。

## Webhook Schema

当前外层结构：

```json
{
  "backend": "local-broker",
  "request": {
    "capability_type": "command",
    "request_id": "tool-sandbox-approve-date-...",
    "command": "date",
    "args": ["date"],
    "caller": "session",
    "intercept_rule": "<catch-all>",
    "reason": null,
    "child_pid": 68382,
    "session_id": "07a1cbd2"
  }
}
```

当前 variant：command、endpoint、capability、network。准确 DTO 应从本地 nono 源码与真实 CLI 输出生成 fixture，而不是靠手写猜测。

## 字段来源与可信度

- `backend`：来自 profile 选中的 backend 名称；receiver 只能视为 claimed backend；
- Command：`child_pid` 来自 shim Unix socket peer credentials，`caller` 来自宿主进程关系，`intercept_rule` 来自 profile，command identity 经过 shim fd/inode 校验；
- Capability：显式 Supervisor IPC 与 seccomp-notify 共用 variant；前者部分字段可能由 child 提供，wire 没有 origin discriminator；
- Endpoint：route/upstream/rule label 来自 proxy/profile，method/path 来自 proxy 观察；当前 child PID 为 `0`、session ID 为 `proxy`；
- Network：源码有类型与测试，但调查时未找到生产构造点。

webhook 当前缺少：

- resolved executable path/identity；
- cwd；
- profile identity/digest；
- supervisor PID；
- session display name；
- Agent entrypoint；
- 统一 observed child PID；
- 最终 resolved deadline。

nono-approval 不得猜测这些字段。

## 同步连接与取消缺陷

webhook backend 是同步长连接。当前 request body 不携带 resolved timeout，也没有独立 cancellation message。

更严重的是，nono 外层 timeout 可能遗弃仍运行的 backend，导致原操作已 fail closed 后，HTTP 连接和 backend 仍继续存在。因此 receiver 无法依赖连接断开判断请求结束，只能使用本地 deadline 作为唯一 Approval Lease。

对应本地上游 issue 草稿：

- `/home/yewfence/code/issues/nolabs-ai__nono___issue_3.md`

## Linux Pathname AF_UNIX

官方 Landlock 文档说明 `linux.af_unix_mediation: "pathname"` 默认关闭是兼容性设计，不是最小权限实现错误。启用后，pathname AF_UNIX 采用 default-deny，需要显式 `filesystem.unix_socket*` grant。

相关源码与文档：

- `../nono/docs/cli/internals/landlock.mdx`；
- `../nono/crates/nono/src/sandbox/linux.rs`。

真实探针证明：默认 profile 下，即使目录 `0700`、socket `0600`，同 UID sandbox 仍能连接；`/proc/net/unix` 还可能暴露随机 socket path。因此随机 pathname 不是秘密，也不能代替 AF_UNIX mediation。

## macOS Unix Socket

macOS Seatbelt restricted network mode 会按显式 Unix socket grants 放行；AllowAll 不提供所需隔离。需要同 UID 防自批准时，应使用 Blocked 或 ProxyOnly 等受限模式，并验证最终 profile 的真实行为。

## Profile Introspection 缺陷

`nono profile show --json` 标称输出 fully resolved profile，但当前遗漏六类 `filesystem.unix_socket*` 字段。resolver 确实合并这些字段，而 JSON 输出没有序列化它们；manifest 又会跳过当时不存在的 socket path，不能完整替代 resolved profile API。

对应本地上游 issue 草稿：

- `/home/yewfence/code/issues/nolabs-ai__nono___issue_4.md`

因此 MVP 的 `config validate` 使用真实短生命周期 sandbox 探针，而不是只做静态 JSON 校验。

## Session Hooks

`nono run --profile ...` 会在宿主侧执行最终 profile 的 session hooks。after hook 不能作为自动安全校验：

- 它在风险窗口之后；
- 它在宿主而非被测 sandbox 中执行；
- child profile 可以覆盖 inherited hook；
- 同 profile 再启动 probe 可能递归。

真实 validation 可能额外执行一次用户 hook，这是命令必须提示的已知副作用。

## 相关上游 Issue

- [#47 interactive permission mode](https://github.com/nolabs-ai/nono/issues/47)：早期 Allow once/Profile/Deny/Quit 设想；
- [#842 Grant permission on the fly](https://github.com/nolabs-ai/nono/issues/842)：确认 ApprovalBackend 与外部人工审批方向；
- [#436 approval model with OPA/Rego](https://github.com/nolabs-ai/nono/issues/436) 与 [#879](https://github.com/nolabs-ai/nono/issues/879)：外部策略引擎，不是本地人工队列；
- [#1198 Capability elevation freezes](https://github.com/nolabs-ai/nono/issues/1198)：terminal approval 冻结与乱码；
- [#1500 Route unmatched proxy destinations through ApprovalBackend](https://github.com/nolabs-ai/nono/issues/1500)：统一运行时决策入口，但未提供本地 daemon。

截至调查日期，没有发现直接覆盖“本地 approval daemon + pending queue + interactive/list/show/approve/deny”的上游实现。
