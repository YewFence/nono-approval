# Protocol and Adaptation

This document is the source of truth for the current Wire Adapter, webhook ingress, and Unix-socket control interface. It describes the JSON the code actually parses and returns; for external facts about nono `0.69` see the [research notes](../research/nono-0.69.md).

## Wire Adapter

The production binary does not depend on the `nono` crate. The in-project `KnownApprovalRequest` covers the four variants the current implementation supports:

```text
command
endpoint
capability
network
```

The outer envelope is:

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

`backend` must be a non-empty string, and `request` must deserialize into a known variant. Known variants allow extra fields; extra fields never enter `KnownApprovalRequest`, ordinary display, or debug responses, and unknown outer fields do not affect parsing either. `raw_request` is only kept in memory while the Broker request is pending; Debug Capture records the known Wire DTO, not the raw JSON. The raw request can never bypass known-DTO validation to become an approval channel.

Shared identity fields:

```rust
request_id: String
session_id: String
```

Neither may be empty. The remaining required fields are validated per variant:

| variant | required fields | ordinary display fields |
| --- | --- | --- |
| `command` | `command`, `caller`, `intercept_rule` | Command, Requested by, Caller, Rule, Reason |
| `endpoint` | `route_id`, `upstream`, `method`, `path`, `rule_label` | Endpoint, Route, Upstream, Rule, Reason |
| `capability` | `path` | Path, Access, Reason |
| `network` | `host` | Destination, Protocol, Resolved IPs, Reason |

`access` may only be `Read`, `Write`, or `ReadWrite`; `protocol` may only be `tcp` or `udp`. Wire fields such as `child_pid`, `session_id`, and `request_id` stay in the internal DTO, but ordinary display only uses the fields in the table above.

## Webhook request

The listener defaults to `127.0.0.1:17443`; the address can be overridden by config or `serve --webhook-listen`, but must be a loopback IP. The path is fixed:

```text
POST /v1/webhooks/approval
Content-Type: application/json
```

The implementation only accepts the exact `application/json` content type, not missing content types, other media types, or parameterized variants. A wrong method returns `405`; a wrong path returns `404`.

Body reads use the configured hard limit, `256 KiB` by default; over the limit, reading stops and the response is:

```http
413 Payload Too Large
{"error":"request body is too large"}
```

Body transport errors, invalid JSON, an empty backend, unknown/incomplete variants, or empty required fields return:

```http
400 Bad Request
{"error":"invalid webhook request"}
```

After the Wire Adapter constructs the safe display detail, the detail JSON may not exceed `1 MiB` by default; over the limit:

```http
422 Unprocessable Entity
{"error":"approval detail is too large"}
```

None of the ingress failures above ever enter Broker pending, the replay cache, or Debug Capture.

## Webhook processing flow

```text
validate method/path/content-type
    -> read body with hard limit
    -> parse envelope and known variant
    -> validate identity and display fields
    -> build sanitized detail and enforce 1 MiB limit
    -> evaluate Session Rules: hit returns granted/denied immediately
    -> reject duplicate (session_id, request_id)
    -> enforce per-session/global capacity
    -> generate approval_id and daemon deadline
    -> register pending request
    -> await Broker decision or Lease expiry
    -> serialize granted/denied response
```

Duplicate requests return `409 Conflict`; a full per-session queue returns `429 Too Many Requests`; a full global queue returns `503 Service Unavailable`; a Broker registration failure returns `500 Internal Server Error`.

Capacity checks apply only after a rule miss. Rule hits have no approval ID, pending entry, or Tombstone, but they run the same duplicate check as registration and reserve the replay key for the tombstone TTL, so a repeated identity returns `409 Conflict` exactly like an unmatched duplicate. Both evaluation and unmatched registration hold the same Broker lock as rule mutation. A shutting-down Broker returns denial instead of applying an allow rule.

Webhook callers are not authenticated: any local process on loopback can submit a well-formed forged request or consume capacity. Ingress itself grants no control interface authority; owner/peer UID rules for the control socket are in [Security model](security.md).

## Webhook response

Human approval or an allow Session Rule hit:

```http
200 OK
{"decision":"granted"}
```

Human denial, a deny Session Rule hit, Lease expiry, or daemon shutdown:

```http
200 OK
{"decision":"denied","reason":"..."}
```

`cancelled` means the handler has already been dropped and no decision can be sent to nono anymore. nono transport errors and all non-`2xx` ingress errors fail closed within nono itself.

## Control transport

The control interface is HTTP over a Unix socket; no TCP management port is opened. The default socket is resolved by `directories::ProjectDirs`:

```text
ProjectDirs.runtime_dir()/control.sock
```

When the current platform has no runtime directory, it falls back to `ProjectDirs.data_local_dir()/runtime/control.sock`. `--control-socket` can specify the path explicitly for both the daemon and clients. The path must fit the target platform's `sockaddr_un.sun_path` length limit; parent directory and socket permission requirements are in [Security model](security.md).

## Control API

All control responses are JSON. Connections are peer-UID-checked first; a connection that fails verification is dropped before reaching the HTTP handler.

### `GET /v1/status`

Returns:

```json
{
  "version": "0.1.0",
  "uptime_seconds": 12,
  "pending": 1,
  "session_rule_count": 0,
  "max_pending": 64,
  "max_per_session": 8,
  "webhook_listen": "127.0.0.1:17443",
  "debug_capture": {"state":"disabled"}
}
```

`debug_capture.state` is `disabled`, `enabled`, or `failed`. When enabled it carries the managed file `path`; when failed it carries a non-sensitive `error_category`. This endpoint is stateless, creates no requests, and is the only endpoint the Profile Validation probe calls.

### `GET /v1/approvals`

Returns only pending requests, ordered by `received_at` ascending, then stably by full approval ID:

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

The API returns the full summary; the CLI and TUI truncate navigation text to the available width.

### `GET /v1/approvals/{approval-id}`

The approval ID must be `appr_` plus 16 lowercase hex characters. The top-level structure of a pending response:

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

Debug metadata is not returned by default. With the exact query `?debug=true`, the response additionally returns `claimed_backend`, `source_kind`, and the known `wire_request`. Raw JSON, unknown extra fields, HTTP headers, and provenance that cannot be reliably derived from the wire are not returned.

A Tombstone still in retention returns:

```json
{
  "status": "completed",
  "approval_id": "appr_7d8f2c6a1b3e4f50",
  "state": "granted",
  "completed_at": "2026-07-27T12:00:03Z"
}
```

Unknown or evicted IDs return `404`; an invalid ID shape returns `400`.

### `POST /v1/approvals/{approval-id}/decision`

Approve:

```json
{"decision":"granted"}
```

Deny:

```json
{"decision":"denied","reason":"outside this task"}
```

The reason must be non-empty, must not consist entirely of NUL characters, and must be at most `4 KiB` after UTF-8 encoding; a reason with embedded NULs may enter the Broker and is safely escaped for display or Debug Capture. Validation failure returns `400`.

Success response:

```json
{"approval_id":"appr_7d8f2c6a1b3e4f50","state":"granted"}
```

Completed or expired requests return `409 Conflict`; unknown IDs return `404`. Decisions accept only full IDs and never act on another request because of a prefix, queue position, or ID reuse.

The hard limit for control request bodies is `8 KiB`; oversized or unparsable decisions return `400`.

### `POST /v1/approvals/{approval-id}/session-rule`

Decides the still-pending source request and remembers its capability path in one atomic operation:

```json
{"action":"deny","scope":"directory","path":"/path/to/project","reason":"outside this task"}
```

`action` is `allow` or `deny`; `scope` is `path` or `directory`. Both fields are required. `path` is optional: absent or null preserves derive-from-source behavior; an explicit string edits the literal rule path. The daemon requires that path/scope to cover the still-pending source. The exact access mode is always inherited; clients cannot supply access or session ID overrides. `reason` is optional for deny (default `denied by local user`) and must be absent or null for allow. Unknown fields are rejected. See [Session Rules](session-rules.md) for matching and replacement semantics.

Success returns the same `DecisionResponse` as `/decision`. Unknown IDs return `404`; completed/expired IDs and exhaustion of the 128-rule limit return `409`. Non-capability requests, invalid paths, paths/scopes that do not cover the source, invalid reasons, unknown enum values/fields, and bodies over `8 KiB` return `400`. A failed operation does not install a rule or decide the source request; expired leases still expire normally. New clients omit the path field when using the original source unchanged through the CLI. Older daemons reject explicit edited-path bodies as unknown fields rather than silently granting a different scope.

### `GET /v1/session-rules`

Lists every rule currently held by the daemon. The owner-authenticated response is:

```json
{"rules":[{"action":"allow","path":"/work/project","scope":"directory","access":"read_write"}]}
```

`action` is `allow` or `deny`; `scope` is `path` or `directory`; `access` is `read`, `write`, or `read_write`, lowercase unlike the webhook wire spelling. An empty rule set returns `{"rules":[]}`. The TUI `S` save flow reads this listing before writing a new TOML policy file.

### `PUT /v1/session-rules`

Atomically replaces the whole rule set with one validated batch. The owner-authenticated request is:

```json
{"rules":[{"action":"allow","path":"/work/project","scope":"directory","access":"read_write"}]}
```

Every rule passes the same path, scope, and access validation as the per-approval session-rule route. Duplicates inside the batch and batches larger than the 128-rule limit return `400`; bodies over `3,276,800` bytes return `413`. A failed request never changes the installed rules. Success responds with the installed count:

```json
{"installed":2}
```

An empty batch clears all rules. The CLI `--policy <file>` load validates a TOML file client-side and sends this request, so it inherits the same atomic replacement semantics.

### `DELETE /v1/session-rules`

Clears every daemon-lifetime rule without affecting pending requests. The owner-authenticated response is:

```json
{"cleared":2}
```

Repeated clearing succeeds with `cleared: 0`. `GET /v1/status` exposes the current `session_rule_count`; the current management surface has no per-rule deletion endpoint.

## Compatibility

- `WIRE_ADAPTER_VERSION` is currently `1` and is written into Tombstones and Debug Capture;
- current tests pin behavior with JSON fixtures for the four variants and keep the nono crate out of the production dependency graph;
- extra fields on known variants stay compatible and are ignored;
- unknown variants, unknown enum values, or missing required display fields fail closed;
- `schema_version` belongs only to this project's config and Debug Capture; the nono webhook is never assumed to carry a version field of its own.
