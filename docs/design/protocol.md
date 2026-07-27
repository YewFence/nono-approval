# 协议与适配边界

本文是 nono-approval 协议设计的唯一事实来源，覆盖 nono webhook Wire Adapter、loopback ingress 和 owner-only control API。nono `0.69.0` 的来源调查与字段可信度见 [nono 0.69 调研](../research/nono-0.69.md)。

## Wire Adapter

发布二进制不依赖 `nono` crate，而是在项目内定义覆盖当前 webhook schema 的最小 wire DTO：

```text
raw request JSON
    ├── parse common metadata: request_id/session_id/capability_type
    └── strictly parse known local wire variant
```

设计约束：

- 外层 request 同时保留为 `RawValue`，仅用于兼容性诊断和未知字段保留；
- 已知 variant 允许未知附加字段，避免 nono 新增兼容字段时直接失败；
- 未知 `capability_type` 或已知 variant 缺少安全展示所需字段时 fail closed；
- raw JSON 不能绕过 wire validation 成为审批通道；
- 生产依赖图不引入 nono；兼容性测试可以引用目标版本的 `nono::supervisor::ApprovalRequest` 生成 fixture。

支持的已知 variant：

```text
command
endpoint
capability
network
```

返回 nono 时生成当前兼容的简单响应，不依赖 nono Rust enum 的派生序列化形状。

## Webhook Envelope

nono 发出：

```http
POST /v1/webhooks/approval HTTP/1.1
Content-Type: application/json
User-Agent: nono-cli/0.69.0
```

外层结构：

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

第一层严格要求：

- `backend` 是非空字符串；
- `request` 是 JSON object；
- 请求体没有 trailing JSON；
- 请求体大小不超过默认 `256 KiB` 的配置上限。

公共 metadata 至少包含：

```rust
struct RequestMetadata {
    capability_type: String,
    request_id: String,
    session_id: String,
}
```

公共字段缺失、未知 variant、已知 variant 解析失败或 body 不合法时返回 `400`，请求不进入 pending。读取过程中一旦确认 body 超过 `256 KiB` 上限，立即停止读取并返回 `413 Payload Too Large`，不得继续解析。

已知 Wire DTO 完成终端安全转义并构造完整决策详情后，总 UTF-8 大小不得超过 `1 MiB`。超限请求返回 `422 Unprocessable Content`，不进入 pending、replay index、普通日志或 Debug Capture；不能截断详情后让用户审批。control API 的单个详情 JSON response 同样不得超过 `1 MiB`。

## Webhook Response

批准：

```http
HTTP/1.1 200 OK
Content-Type: application/json

{"decision":"granted"}
```

拒绝：

```http
HTTP/1.1 200 OK
Content-Type: application/json

{"decision":"denied","reason":"user denied request"}
```

daemon 的 Approval Lease 到期时同样返回 denied，而不是让 handler 无限悬挂。请求体错误、鉴权路径错误、容量超限等 ingress 错误使用非 `2xx`；nono 会 fail closed。超限 body 不进入普通日志、Debug Capture、replay index 或 pending store，只允许记录状态码、已读字节数上限和错误类别。

## Webhook Listener

默认监听固定 loopback 地址：

```text
127.0.0.1:17443
```

端口被占用时启动失败，不能静默换成动态端口，因为 nono profile 中的 webhook URL 是静态配置。用户可以显式覆盖端口，但必须同步修改 profile。

默认拒绝：

```text
0.0.0.0
[::]
任何非 loopback IP
```

webhook 使用固定 path：

```text
/v1/webhooks/approval
```

其他 path 返回 `404`。固定 endpoint 不认证 caller：任意能访问 loopback 端口的本地进程都可以提交形状合法的伪造请求或消耗 pending 容量。daemon 不执行请求中的操作，decision 只返回给提交该精确 webhook 的连接，因此伪造请求不能批准或执行另一条真实 nono 操作；风险限于用户诱导、界面骚扰和 fail-closed 拒绝服务。

## Webhook 处理流程

```text
validate method/path/content-type
    -> read body with hard limit
    -> parse envelope and common metadata
    -> parse known variant
    -> reject duplicate (session_id, request_id)
    -> enforce per-session limit 8 and global limit 64
    -> generate approval_id and local deadline
    -> register pending request
    -> await decision without holding broker lock
    -> serialize granted/denied response
```

HTTP disconnect 仅用于 best-effort 提前清理，不能定义请求是否仍有效；唯一权威结束边界见 [审批生命周期](approval-lifecycle.md)。

容量检查发生在 approval ID 生成、pending 登记与 Debug Capture 写入之前。指定 session 已有 `8` 个 pending request 时返回 `429 Too Many Requests`；全局已有 `64` 个 pending request 时返回 `503 Service Unavailable`。两者都不驱逐、拒绝或缩短已有请求，也不创建 replay/Tombstone 记录。

## Control Transport

管理面使用 HTTP over Unix socket：

```text
$XDG_RUNTIME_DIR/nono-approval/control.sock
```

这样 server 与 CLI/TUI 可以复用 serde DTO 和 HTTP 语义，同时不开放 TCP 管理端口。实现使用 `hyper` 驱动 `tokio::net::UnixStream`，不引入额外 Web 框架。

socket 权限和 peer identity 要求见 [安全模型](security.md)。

## Control API

### `GET /v1/status`

返回 daemon 版本、运行时长、pending 数量、队列上限、webhook listener 摘要和 Debug Capture 是否启用。该接口无状态，不创建或决定审批，也是 Profile Validation 探针唯一允许调用的接口。

### `GET /v1/approvals`

默认只返回 pending 摘要：

```json
{
  "approvals": [
    {
      "approval_id": "appr_7d8f2c6a1b3e4f50",
      "capability_type": "command",
      "summary": "gh repo create demo --private",
      "received_at": "2026-07-27T12:00:00Z",
      "deadline": "2026-07-27T12:04:30Z"
    }
  ]
}
```

响应中的 approvals 按 `received_at` 升序稳定排序；时间相同时使用完整 approval ID 作为确定性 tie-breaker。这样所有 CLI/TUI 客户端观察到相同 FIFO 顺序。

summary 是导航字段，可以在显示客户端按终端宽度截断；control API 返回受协议字段上限约束的完整 summary，不根据 server 终端宽度预截断。

### `GET /v1/approvals/{approval-id}`

返回单个 pending request 的明文决策详情。正常响应只包含操作本身、必要规则上下文和 Approval Lease，不包含 backend、nono request ID、session ID、child PID、raw JSON 或未知字段。

决策详情不得做语义截断。客户端按可用宽度自动换行，并在换行后的内容超过视口高度时提供纵向滚动。

显式调试视图：

```text
GET /v1/approvals/{approval-id}?debug=true
```

调试响应返回完整已知 Wire DTO 和既有来源模型中的技术字段，仍不返回 raw JSON 或未知附加字段。Debug Capture 是否启用只决定这些信息是否落盘，不影响 pending request 的调试查询。

### `POST /v1/approvals/{approval-id}/decision`

```json
{"decision":"granted"}
```

或：

```json
{"decision":"denied","reason":"repository creation is outside this task"}
```

自定义 denial reason 必须是非空 UTF-8 字符串，编码后最多 `4 KiB`。空值、仅包含零字节的非法输入或超限内容返回 `400 Bad Request`，不得静默截断、替换或提交部分理由。固定的快速拒绝理由同样通过该字段传递。

成功响应：

```json
{"approval_id":"appr_7d8f2c6a1b3e4f50","state":"granted"}
```

请求必须使用完整 approval ID 精确匹配。请求已结束、已过期或被其他客户端决定时返回 conflict/not-found 语义，绝不能把决定应用到其他 pending request。

合法 ID 形状固定为 `appr_` 加 16 位小写十六进制字符。control API 不接受大写、缺少前缀、长度错误或其他编码形式。

## 兼容性策略

- 用 command、endpoint、capability、network 四类真实 fixture 固定 wire 行为；
- fixture 与目标 nono 版本类型做兼容性对照；
- 新增未知字段不应破坏已知 variant；
- 新增 variant 必须在展示与安全语义明确后才能接受；
- `schema_version` 只用于本项目控制 DTO 和 Debug Capture，不假设 nono webhook 自带版本字段。
