# Approval Lifecycle

This document describes the approval ID, in-memory state, Approval Lease, Tombstone, replay protection, and shutdown behavior of the current Broker implementation. For HTTP fields and status codes see [Protocol and adaptation](protocol.md).

## One-shot decision

Every valid webhook ingress creates an independent pending request. The Broker only accepts two human decisions:

```text
Approve exactly this request once
Deny exactly this request once
```

A decision must carry the full approval ID. When two clients decide the same request concurrently, the one that holds the Broker lock and removes the pending entry first wins; the later decision sees the Tombstone and gets `NotPending`, never acting on another request.

## Approval ID

The approval ID has a fixed shape:

```text
appr_7d8f2c6a1b3e4f50
```

An ID consists of `appr_` plus the lowercase hex encoding of 8 OS-random bytes. Generation checks the current pending store and not-yet-evicted Tombstones; on collision it regenerates instead of overwriting an existing record.

nono's `request_id` is not used directly as the control primary key. The original request ID only serves the replay key, Debug Capture, and explicit wire debug metadata. The approval ID is not an authentication credential; control authority comes from the Unix socket and peer identity.

The CLI and the control interface reject prefixes, uppercase hex, a missing `appr_`, and other encodings.

## In-memory model

The Broker uses one state guarded by a `tokio::sync::Mutex`:

```text
pending:    approval ID -> PendingApproval
replay:     (session ID, request ID) -> expiration Instant
tombstones: completion order -> Tombstone
```

The lock only covers synchronous state checks and transitions and is never held across `.await`. Each pending request has its own oneshot; the webhook handler waits for the decision or Lease expiry via `Submission::wait`.

PendingApproval holds in memory:

- the approval ID, claimed backend, session ID, request ID, and capability type;
- the known Wire DTO, the raw request JSON, and the constructed safe display detail;
- the wall-clock received/deadline, plus the authoritative monotonic-clock deadline;
- the oneshot sender that delivers the decision to the webhook handler.

## State transitions

```text
                    approve
                 ┌────────────> Granted ──> HTTP 200 granted
                 │
Received ──> Pending
                 │
                 ├── deny ────> Denied ───> HTTP 200 denied
                 ├── timeout ─> Expired ──> HTTP 200 denied
                 ├── handler dropped ─────> Cancelled
                 └── daemon shutdown ─────> Denied ──> HTTP 200 denied (best effort)
```

The implementation keeps no repeatable-transition state field on the pending object; upon reaching a terminal state it removes the object from the pending map, sends the oneshot decision at most once, and creates a Tombstone. Terminal state can therefore never transition again.

Decision source and denial reason per terminal state:

| Terminal state | Source | Decision returned to nono |
| --- | --- | --- |
| `granted` | control approve | `{"decision":"granted"}` |
| `denied` | control deny | user reason or fixed reason |
| `denied` | daemon shutdown | `approval daemon is shutting down` |
| `expired` | daemon deadline | `approval request expired` |
| `cancelled` | webhook handler/Submission dropped | no decision sent |

## Approval Lease

The daemon's `tokio::time::Instant` deadline is the sole Approval Lease for deciding a request. It is never extended or replaced by:

- the HTTP connection still existing;
- the control interface being polled;
- the TUI's wall-clock countdown refreshing;
- the request still existing in an old client snapshot;
- nono's outer backend timeout not yet firing.

The default daemon Lease is `270s`; the nono backend and approval defaults timeout printed by `setup` are both `300s`. The extra 30 seconds only leave headroom for best-effort delivery of an explicit denial after Lease expiry. The webhook body carries no final resolved deadline from nono, so after overriding either side you must keep the nono timeout greater than the daemon timeout yourself.

`list`, `show`, and `decide` also expire requests that have passed their deadline first when reading state. An expired request can never be approved, even if a client holds an old snapshot.

## Disconnects

nono `0.69` has no separate cancellation message, and the outer timeout may abandon a still-running webhook backend. A TCP disconnect is therefore not an authoritative end condition.

When the webhook handler future is dropped, `Submission::drop` best-effort asynchronously cancels the corresponding request and creates a `cancelled` Tombstone. This is only early in-memory cleanup; when no disconnect is observed, the monotonic-clock Lease still expires the request eventually.

## Capacity

Broker default limits:

- global pending: `64`;
- per-session pending: `8`.

When the same session hits its limit, a new ingress gets `429 Too Many Requests`; when the global limit is hit it gets `503 Service Unavailable`. Capacity checks run before approval ID generation, pending registration, and Debug Capture; they never evict, reject, or shorten existing requests.

Request body and display detail size limits belong to the Wire Adapter, see [Protocol and adaptation](protocol.md#webhook-processing-flow).

## Tombstone and detail lifecycle

When a request reaches any terminal state, the PendingApproval is removed from the map and the Wire DTO, raw JSON, display detail, and raw identifiers are destroyed. Normal mode never persists these fields.

The Tombstone keeps internally:

```text
approval_id
capability_type
terminal_state
received_at / completed_at / wait_duration
response_delivery_outcome
keyed hash of claimed backend / session_id / request_id
wire adapter version
```

The current `response_delivery_outcome` is only `not_observed`: the implementation does not yet track whether nono actually received the HTTP response.

Tombstones keep at most `1024` entries for at most `10` minutes by default; whichever limit comes first evicts. `list` only returns pending; `show` returns the approval ID, terminal state, and completion time for full IDs still within retention, never restoring request details. Unknown IDs or evicted Tombstones return `404`.

Explicit Debug Capture is the exception to the no-details-on-disk boundary; see [Security model](security.md#debug-capture).

## Replay protection

Active requests and the replay cache use:

```text
(session_id, request_id)
```

A new ingress gets `409 Conflict` when the same combination is already pending or completed within the last 10 minutes. The replay cache and the Tombstones share the same TTL, but they are two separate structures: the former blocks duplicate ingresses, the latter explains recently completed approval IDs.

## Shutdown and crashes

When the daemon receives `SIGINT` or `SIGTERM`, or when the webhook/control server task ends unexpectedly:

1. The Broker turns all pending requests into `denied`;
2. handlers that still have a oneshot receiver get `approval daemon is shutting down`;
3. it waits `100ms` for best-effort response writes;
4. it aborts the webhook and control server tasks;
5. it removes the control socket and exits.

There is no persisted recovery for process crashes. The nono webhook transport fails and fails closed; the Broker restarts from empty state.
