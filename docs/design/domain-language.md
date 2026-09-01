# Domain Language

This page defines the shared vocabulary nono uses when delegating one-shot human approval to a local external process. Topical docs and implementation module names should use these terms, so one concept never accumulates several names.

## Webhook Endpoint

The local HTTP address where nono submits Approval Requests. The path is fixed; the listener host/port comes from the config or an explicit CLI override. Changes must be mirrored in the nono profile.

_Avoid_: Dynamic URL, token path.

## Setup

The initialization operation that explicitly creates or verifies the local config, prints the current Webhook Endpoint, and prints the nono integration snippet.

_Avoid_: first-run initialization, automatic setup.

## Profile Validation

A non-mandatory check that starts a short-lived sandbox with a user-specified nono profile and probes actual control socket reachability inside it. The result only describes this check and does not prove that later agent launches using the same config behave identically.

_Avoid_: static profile validation, profile enforcement, launch wrapper, after-hook validation.

## Wire Adapter

The compatibility seam that converts nono webhook JSON into locally known approval requests, without depending on nono's runtime Rust types.

_Avoid_: shared ApprovalRequest type, raw JSON approval.

## Approval Lease

The finite validity window during which an Approval Request may be decided, delimited solely by the daemon's monotonic-clock deadline. The HTTP connection, a local wall-clock countdown, and control polling are not Leases.

_Avoid_: connection lifetime, webhook timeout, request age.

## Tombstone

The minimal non-sensitive record kept after a request completes, to explain short-lived state. It lets `show` return the approval ID, terminal state, and completion time, but contains no request details.

_Avoid_: completed request detail, approval history, audit record.

## Ephemeral Approval Detail

The known request fields kept in memory and returned through the owner-only control interface while the approval is still pending, to help the user decide. Destroyed immediately when the request reaches a terminal state.

_Avoid_: redacted approval, approval history, persisted request.

## Debug Capture

The pattern where, once explicitly enabled, each daemon start creates an NDJSON file in the project-managed owner-only state directory and appends `request_received` and `request_completed` plaintext diagnostic events. It is not default behavior and not an authoritative audit log; files are removed only by an explicit `debug clean`.

_Avoid_: default logging, audit log, implicit persistence.
