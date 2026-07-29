# 审批生命周期

本文描述当前 Broker 实现中的 approval ID、内存状态、Approval Lease、Tombstone、
重放保护和关闭行为。HTTP 字段与状态码见[协议与适配](protocol.md)。

## 一次性决定

每个合法 webhook ingress 会建立一条独立 pending request。Broker 只接受两种人工决定：

```text
Approve exactly this request once
Deny exactly this request once
```

决定必须携带完整 approval ID。两个客户端并发决定同一请求时，持有 Broker 锁并先移除
pending 项的一方成功；后续决定看到 Tombstone 后得到 `NotPending`，不会作用于其他请求。

## Approval ID

approval ID 的固定形状为：

```text
appr_7d8f2c6a1b3e4f50
```

ID 由 `appr_` 加 8 个 OS 随机字节的小写十六进制编码组成。生成时检查当前 pending
store 和尚未淘汰的 Tombstone；碰撞就重新生成，不覆盖已有记录。

nono `request_id` 不直接作为 control 主键。原始 request ID 只用于 replay key、
Debug Capture 和显式 wire debug metadata。approval ID 不是认证凭据；control 权限由 Unix
socket 与 peer identity 提供。

CLI 和 control interface 不接受前缀、大写十六进制、缺少 `appr_` 或其他编码形式。

## 内存模型

Broker 使用一份由 `tokio::sync::Mutex` 保护的状态：

```text
pending:    approval ID -> PendingApproval
replay:     (session ID, request ID) -> expiration Instant
tombstones: completion order -> Tombstone
```

锁只覆盖同步状态检查与转换，不跨 `.await` 持有。每条 pending request 有独立 oneshot，
webhook handler 通过 `Submission::wait` 等待决定或 Lease 到期。

PendingApproval 在内存中保存：

- approval ID、claimed backend、session ID、request ID 和 capability type；
- 已知 Wire DTO、原始 request JSON 和已构造的安全展示详情；
- wall-clock received/deadline，及作为权威的单调时钟 deadline；
- 向 webhook handler 交付决定的 oneshot sender。

## 状态转换

```text
                    approve
                 ┌────────────> Granted ──> HTTP 200 granted
                 │
Received ──> Pending
                 │
                 ├── deny ────> Denied ───> HTTP 200 denied
                 ├── timeout ─> Expired ──> HTTP 200 denied
                 ├── handler dropped ─────> Cancelled
                 └── daemon shutdown ─────> Denied ──> HTTP 200 denied（尽力交付）
```

实现不在 pending 对象中保留可重复转换的 state 字段；进入终态时直接从 pending map
移除对象，发送至多一次 oneshot 决定，并创建 Tombstone。因此 terminal state 不能再次
转换。

各终态的决定来源和 denial reason 为：

| 终态 | 来源 | 返回 nono 的决定 |
| --- | --- | --- |
| `granted` | control approve | `{"decision":"granted"}` |
| `denied` | control deny | 用户理由或固定理由 |
| `denied` | daemon shutdown | `approval daemon is shutting down` |
| `expired` | daemon deadline | `approval request expired` |
| `cancelled` | webhook handler/Submission 被丢弃 | 不发送决定 |

## Approval Lease

daemon 的 `tokio::time::Instant` deadline 是请求可被决定的唯一 Approval Lease。它不被
以下事件延长或替代：

- HTTP 连接仍然存在；
- control interface 被轮询；
- TUI 的 wall-clock 倒计时刷新；
- 请求仍存在于旧客户端快照；
- nono 外层 backend timeout 尚未触发。

默认 daemon Lease 为 `270s`；`setup` 输出的 nono backend 和 approval defaults timeout
均为 `300s`。额外 30 秒只为 Lease 到期后尽力返回明确 denial 留出余量。Webhook body
不携带 nono 最终 resolved deadline，用户覆盖任一侧后必须自行保持 nono timeout 大于
daemon timeout。

`list`、`show` 和 `decide` 在读取状态时也会先过期已经越过 deadline 的请求。即使
客户端持有旧快照，过期请求也不能再被批准。

## 连接断开

nono `0.69` 没有独立 cancellation message，而且外层 timeout 可能遗弃仍运行的 webhook
backend。TCP 断开因此不是权威结束条件。

当 webhook handler future 被丢弃时，`Submission::drop` 会尽力异步取消对应请求并创建
`cancelled` Tombstone。这只是内存提前清理；没有观察到断开时，单调时钟 Lease 仍会
最终使请求过期。

## 容量

Broker 默认限制：

- global pending：`64`；
- per-session pending：`8`。

相同 session 达到上限时，新的 ingress 返回 `429 Too Many Requests`；全局达到上限时
返回 `503 Service Unavailable`。容量检查先于 approval ID 生成、pending 登记和 Debug
Capture，不驱逐、拒绝或缩短已有请求。

request body 和展示详情的尺寸限制属于 Wire Adapter，见[协议与适配](protocol.md#webhook-校验流程)。

## Tombstone 与详情生命周期

请求进入任一终态后，PendingApproval 被移出 map，Wire DTO、raw JSON、展示详情和原始
标识符随之销毁。正常模式不持久化这些字段。

Tombstone 在内部保留：

```text
approval_id
capability_type
terminal_state
received_at / completed_at / wait_duration
response_delivery_outcome
keyed hash of claimed backend / session_id / request_id
wire adapter version
```

当前 `response_delivery_outcome` 只有 `not_observed`：实现尚不追踪 HTTP response 是否被
nono 实际接收。

Tombstone 默认最多保留 `1024` 条且最多保留 `10` 分钟，任一限制先到即淘汰。`list`
只返回 pending；`show` 对仍在保留期内的完整 ID 返回 approval ID、终态和完成时间，
不恢复请求详情。未知 ID 或已淘汰 Tombstone 返回 `404`。

显式 Debug Capture 是详情默认不落盘边界的例外，见[安全模型](security.md#debug-capture)。

## 重放保护

活动请求和 replay cache 使用：

```text
(session_id, request_id)
```

相同组合已经 pending 或在最近 10 分钟内完成时，新 ingress 返回 `409 Conflict`。replay
cache 与 Tombstone 使用相同 TTL，但它们是两个独立结构：前者阻止重复 ingress，后者
解释最近完成的 approval ID。

## 关闭与崩溃

daemon 收到 `SIGINT`、`SIGTERM`，或 webhook/control server task 意外结束后：

1. Broker 把所有 pending request 转成 `denied`；
2. 对仍有 oneshot receiver 的 handler 发送 `approval daemon is shutting down`；
3. 等待 `100ms` 让响应尽力写出；
4. abort webhook 与 control server task；
5. 删除 control socket 并退出。

进程崩溃时没有持久化恢复。nono webhook transport 失败并 fail closed；重启后的 Broker
从空状态开始。
