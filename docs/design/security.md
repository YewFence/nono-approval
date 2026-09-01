# Security Model

This document describes the current implementation's trust model, control socket, Profile Validation, terminal security, plaintext data lifecycle, logging, and Debug Capture.

## Trust boundary

```text
nono supervisor
    ├── trusted enforcement and validation
    └── sends approvable request
             │
             ▼
nono-approval daemon
    ├── trusted to faithfully return user's decision
    ├── not trusted to weaken nono hard denies
    └── never executes the requested operation itself
```

The daemon never judges whether an operation is safe, never overrides nono's deny, protected roots, or platform sandbox constraints, and only provides a one-shot human decision.

## Webhook and Control separation

- webhook ingress: fixed loopback TCP endpoint `/v1/webhooks/approval`;
- control: owner-only Unix socket; no TCP management port is opened.

The loopback webhook has no peer UID and does not authenticate callers. Any local process can submit forged requests, spam the approval interface, or consume pending capacity; the daemon never executes the operation in a request, and approval is returned only to that exact webhook connection, so a forged request can never be used to approve or execute another real operation. This ingress risk is accepted as a local fail-closed availability constraint; only the control socket uses OS peer credentials to isolate other users.

## Control Socket

The runtime base directory is resolved by `directories::ProjectDirs`, project subdirectories get `0700`, and the socket gets `0600`. If a socket already exists at startup:

- the owner, type, and permissions of the runtime path are verified first;
- a stale socket is cleaned only after confirming the target is a socket owned by the current user with no active daemon;
- regular files, symlinks, wrong owners, or insecure parent directories all fail startup;
- no recursive deletion or broad path cleanup is used.

The socket pathname must fit fully into the target platform's `sockaddr_un.sun_path`; an over-long path fails startup. There is no fallback to a shared `/tmp` or any other location that would weaken the owner-only boundary.

Every control connection must verify the peer UID:

- Linux: `SO_PEERCRED`;
- macOS: `LOCAL_PEERPID` plus `getpeereid`;
- when it cannot be obtained or verified: fail closed, never degrade to file permissions alone.

The platform adapter reads these kernel credentials through `nix`'s safe wrappers. The production crate globally forbids `unsafe` and never calls `libc` directly; if a required call for a target platform is not covered by the safe interfaces of the current `nix`, the implementation for that platform should stop and the dependency choice be re-reviewed — the constraint must never be relaxed ad hoc inside the adapter.

The MVP uses no control bearer token, random socket filename, keyring, or challenge-response.

## Same-UID self-approval boundary

Socket file permissions and peer UID cannot distinguish a same-UID host CLI from a sandboxed agent. The MVP does not force users to adopt a project-generated profile and does not take over agent launches; same-UID self-approval protection is ultimately the user's nono profile and launch method.

On Linux, if pathname Unix sockets need isolation, enable:

```json
{"linux":{"af_unix_mediation":"pathname"}}
```

and avoid adding `filesystem.unix_socket*` grants covering the control socket, its parent directory, or a subtree that covers it. On macOS, use a Blocked or ProxyOnly restricted network mode that can confine Unix sockets, not AllowAll.

## Profile Validation

```bash
nono-approval config validate --profile <name-or-path>
```

This is an explicit diagnostic command, not a launch gate. It starts a short-lived real sandbox through the installed nono and the user-specified final profile, and runs the hidden `__probe-control-socket` inside it.

The probe only connects to the control socket and calls the stateless:

```text
GET /v1/status
```

It never creates an Approval Request and never calls approve/deny.

It returns success only when the parent process has confirmed the probe started inside the sandbox and `connect(2)` received an explicit `EACCES` or `EPERM`. All of the following return non-zero:

- the control socket is reachable or any HTTP response is received;
- the daemon is not running;
- nono is unavailable or the profile is invalid;
- sandbox initialization fails;
- the probe does not start or is denied by a command policy;
- the output protocol is invalid, a timeout occurs, or other connection errors occur.

The probe uses short timeouts and a fixed-version parent/child protocol, reporting at least `started` and `denied(errno)`. ENOENT, a not-started sandbox, or an indeterminate state must never be reported as safe.

A plain `nono run --profile ...` executes the final profile's `session_hooks.before/after` on the host side, so validation may additionally execute the user's hooks once, and the CLI must disclose this clearly. The MVP does not copy or rewrite the profile to remove hooks.

Validation installs no after hook and never relies on hooks as a safety guarantee: the after timing is too late, it runs on the host instead of the sandbox under test, child profiles can override inherited hooks, and probing with the same profile could recurse. The result only proves this behavior under the current nono version, the currently resolved profile, and the current control socket — it does not prove that the user later starts an agent with the same config.

## Request flooding and replay

- webhook body defaults to `256 KiB`, global pending to `64`, per-session pending to `8`, and output has hard limits too;
- an oversized body is rejected with `413` before parsing; the content never enters pending, logs, or Debug Capture;
- a full per-session queue returns `429`, a full global queue returns `503`; capacity rejections never evict existing requests and never enter Debug Capture;
- duplicate `(session_id, request_id)` combinations are rejected;
- a short-lived replay cache is kept after completion;
- webhook ingress itself grants no control authority; whether a same-UID local process can additionally reach the control socket remains a deployment-side sandbox concern;
- non-loopback binds are rejected by default.

## Config parsing

`config.toml` must declare `schema_version = 1` and is parsed with unknown fields rejected. A misspelled security-related field, a missing version, or an unsupported schema makes both `setup` and `serve` return non-zero; invalid explicit config is never silently replaced with defaults. `serve` never migrates or rewrites the config.

Runtime parameters follow `CLI > config.toml > built-in defaults`, and every layer runs the same validation. The fixed webhook path is not part of the config and has no override entry.

## Terminal injection defense

All of the following are untrusted: command, args, path, host, URL, reason, caller, rule label, and session ID.

All CLI/TUI output must:

- strip ANSI escape sequences;
- replace C0/C1 control characters;
- use visible escapes for newlines, tabs, and non-printable characters;
- cap the full sanitized decision detail at `1 MiB`;
- never change the terminal title, emit hyperlinks, or move the cursor;
- use deterministic quoting for display only, never handed back to a shell.

An over-limit detail must reject the whole request at ingress; it must never be truncated, folded, or suffix-omitted so the user decides on incomplete content. List summaries may truncate explicitly because they are for navigation only; control detail and TUI detail must be complete with vertical scrolling, and CLI `show` prints field by field without semantic truncation.

The user-entered denial reason is also an untrusted boundary input: it must be non-empty UTF-8, must not consist entirely of NUL characters, and must be at most `4 KiB` after encoding; a reason with embedded NULs is currently allowed into the Broker. Reasons go through the same safe-escaping rules in terminal output and Debug Capture, and a validation failure must never be truncated and submitted anyway.

## Plaintext display boundary

The MVP does not automatically redact tokens, passwords, signed URLs, or user content. The approver needs to see the complete known operation nono actually requested approval for; heuristic redaction can hide critical differences.

Normal-mode boundary:

- the complete known fields of a pending request may be returned in plaintext over the owner-only control surface;
- control detail JSON contains `source_kind`, and the main CLI/TUI view renders only the operation and necessary rule context;
- plaintext always goes through terminal safe escaping;
- raw JSON and unknown extra fields never enter ordinary or debug views;
- request details are never written to ordinary logs or disk;
- details are destroyed immediately after terminal state, keeping only the Tombstone.

## Provenance model

Field origins within a valid webhook are not uniform, and loopback HTTP cannot prove caller identity. Provenance is used only for parsing, testing, and debugging and must never become an additional authorization basis.

Internally only the display-template `SourceKind` is kept:

```text
tool_sandbox
proxy
capability
network
```

The ordinary CLI/TUI renders no generic trust labels; `--debug` and Debug Capture keep the original values of the known Wire DTO. For field origins and missing information see the [nono 0.69 research](../research/nono-0.69.md).

## Ordinary logs

Default logs only record:

- approval ID;
- capability type;
- a short form of the session ID;
- state transitions;
- wait duration;
- error category.

Full args, path, URL, raw JSON, or denial reasons are never logged by default. nono itself owns the real security audit; daemon logs are for operational diagnostics only.

## Debug Capture

Explicitly enabled with:

```text
--debug-capture
```

The daemon never accepts an arbitrary capture path. Each enablement creates a new `0600` NDJSON file in the project-managed owner-only state directory; the directory must be owned by the current UID with `0700` permissions and contain no symlink path components, otherwise startup fails. The startup banner and `status` must keep showing that Debug Capture is enabled and where the current file lives. Ordinary text/JSON logging policy is unchanged by Debug Capture.

### Format

UTF-8 JSON Lines (NDJSON) append-only writes. Each record carries an integer `schema_version`, is fully serialized into a single line containing no physical newline, and is then appended; no JSON array is maintained or rewritten.

On a process crash, previously complete lines parse independently; readers ignore the single possibly incomplete trailing line.

### Record types

Only two event kinds are written:

- `request_received`: the complete known Wire DTO, existing provenance information, and the daemon's local deadline;
- `request_completed`: approval ID, terminal state, decision source, optional denial reason, wait duration, and the webhook response delivery outcome.

Completion records never repeat the Wire DTO; they link to the received record via the approval ID. Control API polling, list/show, TUI selection and scrolling, and countdown redraws never enter the capture file.

### Provenance fields

Debug Capture reuses the existing provenance model and introduces no second provenance schema:

- the outer `backend` is recorded as `claimed_backend`;
- the request ID, session ID, caller, rule fields, reason, and child PID actually present in the Wire DTO are kept;
- the variant-derived `source_kind` is recorded;
- exact known wire values are preserved.

It never invents or additionally records resolved executable identity, cwd, profile identity/digest, supervisor PID, session display name, agent entrypoint, a unified observed child PID, or nono's resolved deadline — none of which the webhook provides and the daemon cannot reliably confirm. It also never records all HTTP headers, the loopback peer address, platform labels, or control peer information.

### File retention and cleanup

Capture files never auto-rotate, never auto-expire, and are never deleted at daemon start or exit. Every daemon start with capture enabled creates a new file; retaining history is the user's responsibility.

`nono-approval debug captures` only lists the names, creation times, and sizes of valid managed files and never reads their plaintext content. `nono-approval debug clean` explicitly deletes all valid managed files without a second confirmation. Cleanup validates each entry against current-UID ownership, regular-file type, and the managed naming rules; symlinks, subdirectories, and abnormal entries are rejected, deletion is never recursive, and the managed directory itself is never deleted. Any anomaly or deletion failure makes the command return non-zero.

### Runtime failure

When `--debug-capture` cannot safely create the directory or file at startup, the daemon fails to start rather than silently ignoring the explicitly requested capture.

When the first append fails while the daemon is running, this run's Debug Capture is closed immediately and enters an unrecoverable `failed` state, but existing and subsequent approvals keep processing; Debug Capture is not an approval security boundary, and a full disk or broken diagnostic file must never block human decisions. The implementation logs one prominent error only — no endless retries, no switching to a new file, no repeated identical log spam. `status` and the TUI bottom status area keep showing `debug capture: failed` with a non-sensitive error category until the daemon restarts.
