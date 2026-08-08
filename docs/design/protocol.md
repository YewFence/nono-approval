# 协议与适配

本文是当前 Wire Adapter、webhook ingress 和 Unix-socket control interface 的事实来源。
它描述代码实际解析和返回的 JSON；nono `0.69` 的外部事实见[调研记录](../research/nono-0.69.md)。

## Wire Adapter

生产二进制不依赖 `nono` crate。项目内的 `KnownApprovalRequest` 覆盖当前实现支持的四种
variant：

```text
command
endpoint
capability
network
```

外层 envelope 为：

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

`backend` 必须是非空字符串，`request` 必须能反序列化为已知 variant。已知 variant
允许额外字段，额外字段不会进入 `KnownApprovalRequest`、普通展示或 debug response；
外层 unknown field 也不影响解析。`raw_request` 只在 Broker pending 期间的内存中保留；
Debug Capture 记录已知 Wire DTO，不记录 raw JSON。raw request 不能绕过已知 DTO 验证
成为审批通道。

公共身份字段为：

```rust
request_id: String
session_id: String
```

二者不能为空。其余必填字段按 variant 验证：

| variant | 必填字段 | 普通展示字段 |
| --- | --- | --- |
| `command` | `command`、`caller`、`intercept_rule` | Command、Requested by、Caller、Rule、Reason |
| `endpoint` | `route_id`、`upstream`、`method`、`path`、`rule_label` | Endpoint、Route、Upstream、Rule、Reason |
| `capability` | `path` | Path、Access、Reason |
| `network` | `host` | Destination、Protocol、Resolved IPs、Reason |

`access` 只能是 `Read`、`Write` 或 `ReadWrite`；`protocol` 只能是 `tcp` 或 `udp`。
`child_pid`、`session_id`、`request_id` 等 wire 字段会保留在内部 DTO，但普通展示只
使用上表字段。

## Webhook 请求

监听地址默认 `127.0.0.1:17443`，地址可由 config 或 `serve --webhook-listen` 覆盖，但
必须是 loopback IP。path 固定为：

```text
POST /v1/webhooks/approval
Content-Type: application/json
```

实现只接受精确的 `application/json` content type，不接受缺失、其他 media type 或带
参数的变体。方法错误返回 `405`，path 错误返回 `404`。

读取 body 时使用配置的 hard limit，默认 `256 KiB`；超过限制会停止读取并返回：

```http
413 Payload Too Large
{"error":"request body is too large"}
```

body transport error、非法 JSON、空 backend、未知/incomplete variant 或空必填字段返回：

```http
400 Bad Request
{"error":"invalid webhook request"}
```

Wire Adapter 构造安全展示详情后，详情 JSON 大小默认不得超过 `1 MiB`；超限返回：

```http
422 Unprocessable Entity
{"error":"approval detail is too large"}
```

上述 ingress 失败都不会进入 Broker pending、replay cache 或 Debug Capture。

## Webhook 处理流程

```text
validate method/path/content-type
    -> read body with hard limit
    -> parse envelope and known variant
    -> validate identity and display fields
    -> build sanitized detail and enforce 1 MiB limit
    -> reject duplicate (session_id, request_id)
    -> enforce per-session/global capacity
    -> generate approval_id and daemon deadline
    -> register pending request
    -> await Broker decision or Lease expiry
    -> serialize granted/denied response
```

重复 request 返回 `409 Conflict`；per-session 满返回 `429 Too Many Requests`；全局满返回
`503 Service Unavailable`；Broker 注册失败返回 `500 Internal Server Error`。

webhook caller 不认证，loopback 上的本地进程可以提交形状合法的伪造请求或消耗容量。
ingress 本身不授予 control interface 权限；control socket 的 owner/peer UID 规则见
[安全模型](security.md)。

## Webhook response

人工批准：

```http
200 OK
{"decision":"granted"}
```

人工拒绝、Lease 到期或 daemon shutdown：

```http
200 OK
{"decision":"denied","reason":"..."}
```

`cancelled` 表示 handler 已经被丢弃，无法再向 nono 发送决定。nono transport 错误和
所有非 `2xx` ingress 错误都由 nono 自身 fail closed。

## Control transport

Control interface 使用 HTTP over Unix socket，不开放 TCP 管理端口。默认 socket 由
`directories::ProjectDirs` 解析：

```text
ProjectDirs.runtime_dir()/control.sock
```

当前平台没有 runtime directory 时回退到 `ProjectDirs.data_local_dir()/runtime/control.sock`。
`--control-socket` 可以为 daemon 和客户端显式指定路径。路径必须符合目标平台
`sockaddr_un.sun_path` 长度限制，父目录和 socket 权限要求见[安全模型](security.md)。

## Control API

所有 control response 都是 JSON。连接先验证 peer UID；验证失败的连接被丢弃，不会
进入 HTTP handler。

### `GET /v1/status`

返回：

```json
{
  "version": "0.1.0",
  "uptime_seconds": 12,
  "pending": 1,
  "max_pending": 64,
  "max_per_session": 8,
  "webhook_listen": "127.0.0.1:17443",
  "debug_capture": {"state":"disabled"}
}
```

`debug_capture.state` 为 `disabled`、`enabled` 或 `failed`。enabled 时附带托管文件
`path`；failed 时附带非敏感 `error_category`。该接口无状态、不创建请求，是 Profile
Validation probe 唯一调用的接口。

### `GET /v1/approvals`

只返回 pending，按 `received_at` 升序、再按完整 approval ID 稳定排序：

```json
{
  "approvals": [
    {
      "approval_id": "appr_7d8f2c6a1b3e4f50",
      "capability_type": "command",
      "summary": "date",
      "received_at": "2026-07-27T12:00:00Z",
      "deadline": "2026-07-27T12:04:30Z"
    }
  ]
}
```

API 返回完整 summary；CLI 和 TUI 再按当前可用宽度截断导航文本。

### `GET /v1/approvals/{approval-id}`

approval ID 必须是 `appr_` 加 16 位小写十六进制字符。pending response 的顶层结构为：

```json
{
  "status": "pending",
  "approval_id": "appr_7d8f2c6a1b3e4f50",
  "received_at": "2026-07-27T12:00:00Z",
  "deadline": "2026-07-27T12:04:30Z",
  "capability_type": "command",
  "summary": "date",
  "source_kind": "tool_sandbox",
  "fields": [{"label":"Command","value":"date"}]
}
```

默认不返回 debug metadata。精确 query `?debug=true` 时额外返回 `claimed_backend`、
`source_kind` 和已知 `wire_request`。raw JSON、未知附加字段、HTTP headers 和无法从
wire 可靠得到的 provenance 不返回。

Tombstone 仍在保留期内时返回：

```json
{
  "status": "completed",
  "approval_id": "appr_7d8f2c6a1b3e4f50",
  "state": "granted",
  "completed_at": "2026-07-27T12:00:03Z"
}
```

未知或已淘汰 ID 返回 `404`；非法 ID 形状返回 `400`。

### `POST /v1/approvals/{approval-id}/decision`

批准请求：

```json
{"decision":"granted"}
```

拒绝请求：

```json
{"decision":"denied","reason":"outside this task"}
```

reason 必须非空、不能全部由 NUL 字符组成，UTF-8 编码后最多 `4 KiB`；混合 NUL 的
reason 可以进入 Broker，并在展示或 Debug Capture 时安全转义。校验失败返回 `400`。

成功响应：

```json
{"approval_id":"appr_7d8f2c6a1b3e4f50","state":"granted"}
```

已完成或已过期 request 返回 `409 Conflict`；未知 ID 返回 `404`。决定只接受完整 ID，
不会因前缀、队列位置或 ID 重用而作用到其他请求。

Control request body 的 hard limit 为 `8 KiB`；超限或无法解析的 decision 返回 `400`。

## 兼容性

- `WIRE_ADAPTER_VERSION` 当前为 `1`，写入 Tombstone 和 Debug Capture；
- 当前测试以四种 variant 的 JSON fixture 固定行为，不把 nono crate 放入生产依赖图；
- 已知 variant 的额外字段保持兼容并被忽略；
- 未知 variant、未知 enum 值或缺少展示必填字段 fail closed；
- `schema_version` 只属于本项目 config 和 Debug Capture，不假设 nono webhook 自带版本字段。
