# 审批生命周期

本文是 broker、状态机、Approval Lease、并发与内存生命周期的唯一事实来源。

## 架构

```text
nono Tool Sandbox / proxy / supervisor
              │ synchronous webhook
              ▼
┌──────────────── nono-approval serve ────────────────┐
│ WebhookServer -> Broker/PendingStore -> Control API │
└──────────────────────────┬──────────────────────────┘
                           │ owner-only Unix socket
                           ▼
                 CLI / interactive TUI
```

webhook 负责 ingress，Unix socket 负责 control。daemon 不执行请求中的命令，只忠实地把用户针对一个精确请求的一次性决定返回给 nono。

## 一次性决定

MVP 只支持：

```text
Approve exactly this request once
Deny exactly this request once
```

不支持 session 级批准、永久允许、规则生成或自动修改 profile。这些能力必须由 nono 自己执行与审计，daemon 不能成为第二套策略引擎。

## Approval ID

nono `request_id` 用于其内部审计与 replay protection，不直接作为 control 主键。每次 ingress 生成独立、不可预测、固定形状的 ID：

```text
appr_7d8f2c6a1b3e4f50
```

ID 由 `appr_` 加 8 个随机字节编码成 16 位小写十六进制组成。approval ID 不是认证凭据；在最多 64 个 pending 和有限 Tombstone/replay cache 的本地规模下，64-bit 随机空间足够。生成时必须同时检查 pending store、Tombstone 和 replay cache，发现碰撞就重新生成，绝不能覆盖旧记录。

原因：

- 隔离不同 session 中可能重复的 request ID；
- 不信任未来第三方 webhook client 提交的 ID 形状；
- control API 和 UI 使用统一标识；
- 原始 request ID 仍保留在已知 Wire DTO 和调试信息中。

CLI 不支持唯一前缀，所有非交互决定必须完整精确匹配 approval ID。

## 数据模型

```rust
struct PendingApproval {
    approval_id: ApprovalId,
    claimed_backend: String,
    metadata: RequestMetadata,
    raw_request: Box<serde_json::value::RawValue>,
    wire_request: KnownApprovalRequest,
    received_at: std::time::SystemTime,
    deadline: tokio::time::Instant,
    state: ApprovalState,
    decision_tx: Option<tokio::sync::oneshot::Sender<Decision>>,
}
```

`BrokerState` 使用 `Arc<tokio::sync::Mutex<_>>`。锁只覆盖状态检查与转换，绝不跨 `.await` 持有；每个请求使用自己的 `oneshot` 等待最终决定。

状态：

```rust
enum ApprovalState {
    Pending,
    Granted,
    Denied,
    Expired,
    Cancelled,
}
```

## 状态机

```text
                    approve
                 ┌────────────> Granted ──> HTTP 200 granted
                 │
Received ──> Pending
                 │
                 ├── deny ────> Denied ───> HTTP 200 denied
                 ├── timeout ─> Expired ──> HTTP 200 denied
                 ├── observed disconnect ─> Cancelled (best effort)
                 └── daemon shutdown ─────> Denied / Cancelled
```

唯一合法转换：

```text
Pending -> Granted | Denied | Expired | Cancelled
```

状态转换必须在同一把锁下完成。terminal state 不能再次转换。两个客户端同时决定一个请求时，第一个成功，后续请求得到冲突结果，nono 只收到一个决定。

## Approval Lease

daemon 的单调时钟 deadline 是请求可被决定的唯一 Approval Lease。它不由以下事件延长或替代：

- HTTP 连接仍然存在；
- control API 被轮询；
- TUI 本地倒计时刷新；
- 请求仍显示在旧的客户端快照中；
- nono 外层 backend timeout 尚未触发。

当前 webhook body 不携带 nono 最终解析后的 deadline，因此 daemon 默认 `request_timeout` 为 `270s`（4 分 30 秒），`setup` 输出的 nono backend/default timeout 为 `300s`。30 秒差值只为 daemon 在本地 Lease 到期后生成明确 denial 并尝试完成 HTTP response delivery 留出余量，不构成两套 timeout 已同步的保证。

用户可以显式覆盖 daemon timeout，也可以自行修改 nono profile；两者必须由用户保持 `nono timeout > daemon timeout`。当前 webhook 无法让 daemon 在运行时确认 nono 的最终 resolved timeout，Profile Validation 也不能把静态配置关系升级为运行时保证，只能在诊断输出中提醒已知配置关系与限制。

UI 展示 wall-clock deadline 仅供人阅读；批准时始终重新检查 daemon 单调 deadline，不能信任客户端计算。

## 连接断开

nono 当前没有 cancellation message，外层 timeout 还可能遗弃仍运行的 webhook backend，因此不能把 TCP 断开当成可靠结束信号。

handler future 被取消或观察到连接关闭时，可以通过 drop guard 尽力将 pending request 标记为 cancelled；该行为只是提前清理优化。即使永远观察不到断开，Approval Lease 到期后请求也必须不可批准。

## Pending 容量

MVP 必须限制：

- request body 默认 `256 KiB`；
- global pending 默认上限 `64`；
- per-session pending 默认上限 `8`；
- 安全转义后的完整决策详情默认上限 `1 MiB`；
- Debug Capture 运行时写入失败只关闭捕获，不影响审批服务。

容量超限时 ingress fail closed，不进入 pending，也不能驱逐已有请求来给新请求腾位置。per-session 满时返回 `429`，全局满时返回 `503`；容量拒绝发生在 approval ID 生成和 Debug Capture 之前。

## 请求详情生命周期

pending request 的完整已知 Wire DTO 只保存在内存中。请求进入 terminal state 后立即销毁详情，正常模式不提供完成请求详情查询，也不把详情写入日志或磁盘。

完成后只保留最小 Tombstone：

```text
approval_id
capability_type
terminal_state
received_at / completed_at / wait_duration
response_delivery_outcome
keyed hash of backend / session_id / request_id
wire adapter version
```

Tombstone 不保留 command、args、path、URL、reason、原始标识符、child PID 或 raw JSON。默认最多 1024 条或 10 分钟，任一限制先到即淘汰。`list` 不展示 Tombstone；`show` 对刚完成的完整 ID 只返回终态和完成时间。

显式 Debug Capture 是默认不落盘边界的唯一例外，见 [安全模型](security.md)。

## 重放保护

活动请求按以下键建立索引：

```text
(session_id, request_id)
```

相同组合第二次出现时拒绝。请求完成后索引在短期 replay cache 中继续保留，避免完成瞬间被重复提交。

## 关闭与崩溃

收到 `SIGINT` 或 `SIGTERM`：

1. 停止接受新 webhook；
2. 停止接受新 control connection；
3. 将所有 pending request fail closed；
4. 给响应一个短暂 flush 窗口；
5. 删除 control socket；
6. 清空内存状态；
7. 退出。

daemon 崩溃或连接中断时，nono webhook backend 返回错误并 fail closed。重启后不恢复旧请求。
