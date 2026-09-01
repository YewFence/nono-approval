# CLI and TUI

This document describes the user interface of the current `src/cli.rs` and `src/interactive.rs`. For control JSON and status codes see [Protocol and adaptation](protocol.md); for input security constraints see [Security model](security.md).

## Command overview

```text
nono-approval                         # interactive approval interface
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

Running without a subcommand enters the TUI directly. `__probe-control-socket` is a hidden subcommand used by Profile Validation and is not promised as a user interface.

Every approval ID must be the complete `appr_` plus 16 lowercase hex characters already at clap parse time. `show` supports pending details and completed Tombstones still in retention; `approve`/`deny` only accept requests that are still pending.

## `setup`

`setup` resolves the platform-native config path from `ProjectDirs` and creates or verifies:

```text
<ProjectDirs.config_dir()>/config.toml
```

The config file is owned by the current user with `0600` permissions and uses `schema_version = 1`. First creation writes through an atomic temp file; an existing file only gets safe loading and schema/value validation — never overwritten, never migrated.

On success it prints:

1. the config file path;
2. the full endpoint derived from the current `webhook.listen`;
3. the nono JSON snippet for the `local-broker` webhook backend and `approval_defaults`, with a `300s` timeout;
4. a hint about the Profile Validation command.

`setup` does not start the daemon, does not modify the nono profile, and does not create the control socket.

## `config validate`

```bash
nono-approval config validate --profile <name-or-path>
```

Before starting the real probe, the command warns on stderr that the target profile's host-side session hooks may run. The probe must first print `nono-approval-probe-v1 started`, then connect to the control socket inside the sandbox; only errno `1` (`EPERM`) or `13` (`EACCES`) counts as success. Reachable, not started, protocol errors, other errnos, and the 15-second timeout all return non-zero.

The hidden `--control-socket` argument can point the command at a specific path, but the command never creates an approval request and never calls the decision API.

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

`serve` loads the platform config first, then merges runtime values in this order:

```text
explicit CLI arguments > config.toml > built-in defaults
```

CLI overrides still go through the same loopback, positive-number, and `max_per_session <= max_pending` checks. A missing config, insecure permissions, unknown fields, an invalid schema, or invalid values all fail startup; `serve` never runs `setup` implicitly.

Defaults come from the implementation: Lease `270s`, global `64`, per-session `8`, body `256KiB`, detail `1MiB`. The webhook path is fixed, but the listener host/port can be overridden explicitly; after overriding, the nono profile must be updated to match.

A successful daemon start prints the webhook endpoint and the actual control socket. When Debug Capture is enabled it also prints the capture file path for this run.

## `status`

Example:

```text
Daemon: running
Pending: 2
Started: 8s ago
Webhook: 127.0.0.1:17443
Debug capture: enabled (/.../debug-captures/2026-...ndjson)
```

Debug Capture may show `disabled`, `enabled (path)`, or `failed (category)`. A failed state never auto-recovers, but never blocks approvals either.

## `list`

Default human output has exactly three fields:

```text
ID                     TYPE        REQUEST
appr_7d8f2c6a1b3e4f50   command     date
```

`--json` outputs the full `ApprovalList` of the control API. Human summary output truncates to the current terminal width with `…`; API and JSON output never truncate to terminal width. The list only contains pending requests, never Tombstones.

## `show`

```bash
nono-approval show appr_7d8f2c6a1b3e4f50
nono-approval show --debug appr_7d8f2c6a1b3e4f50
```

Pending requests print field by field:

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

`--debug` additionally prints the claimed backend, source kind, and the known Wire DTO. For completed requests whose Tombstone is still in retention, only the approval ID, terminal state, and completion time are printed; details are never restored. Unknown or evicted Tombstones return an error.

Field values are terminal-sanitized first. The CLI does no semantic truncation or horizontal scrolling of its own; long lines wrap normally in the terminal, and only the `list` summary uses an explicit ellipsis.

## `approve` and `deny`

```bash
nono-approval approve appr_7d8f2c6a1b3e4f50
nono-approval deny appr_7d8f2c6a1b3e4f50
nono-approval deny appr_7d8f2c6a1b3e4f50 --reason "outside this task"
```

The command itself is the final decision; there is no second confirmation. `deny` without `--reason` uses the fixed reason `denied by local user`. Reasons must pass the Broker's unified validation: non-empty, not all NUL, and at most `4 KiB` after UTF-8 encoding.

`approve` and `deny` do not support `--latest`, `--all`, operation names, or ID prefixes. Unknown, completed, expired, or already-decided requests fail, never falling back to another queue item.

## Debug Capture commands

```bash
nono-approval debug captures
nono-approval debug clean
```

`debug captures` lists only the names, creation times, and byte sizes of managed files — it never reads their content. `debug clean` deletes every file that passes the owner, regular-file, `0600`, and fixed-naming checks, and never deletes directories recursively; encountering an unsafe entry returns non-zero.

## Shell completion

```bash
nono-approval completions bash
nono-approval completions elvish
nono-approval completions fish
nono-approval completions powershell
nono-approval completions zsh
```

## TUI entry and refresh

```bash
nono-approval
```

The TUI works only through the control client. The initial state is `Disconnected — waiting for daemon…`; while the control socket is unavailable it reconnects every `1s`, and once connected it fetches the list, status, and current detail every `500ms`. With nothing pending it stays open showing a waiting state and never runs `setup` or starts the daemon automatically.

A disconnect immediately clears approvals, selection, detail, detail scroll, and any unsubmitted denial reason. After reconnecting, only data freshly returned by the new daemon is used; old snapshots are never reused across daemon lifetimes.

On refresh, the current selection is preserved by full approval ID first; when the target disappears it falls back to the item still in range at the old index. With no old selection it picks the first item. A detail request that hits completed or not-found clears the detail and never transfers the decision to another item.

## TUI layout and keybindings

At least `90` columns wide it uses a 38%/62% left/right two-pane layout; narrower shows a single pane where `Tab` switches between the queue and the detail, with the queue shown by default. The detail uses ratatui `Wrap` line wrapping and vertical scrolling; fields are never scrolled horizontally or truncated. Switching between two-pane and single-pane never changes the selected approval ID.

```text
j / Down        next request
k / Up          previous request
a               approve immediately
d               deny immediately with a fixed reason
D               open the reason input mode
q               quit the TUI
Ctrl-c          quit the TUI
Tab             switch queue/detail on narrow screens
Ctrl-d/u        scroll detail down/up
PageDown/Up     scroll detail down/up
g / G           detail top/bottom
```

Enter in browse mode has no approval meaning. In reason input mode, Enter submits and Esc discards; the target approval ID is fixed while typing, and if the target completes during the next refresh, submission just shows a failure. `D` and the CLI `--reason` use the same Broker validation rules.

The TUI calls ratatui restore on normal return and in the panic hook, restoring the alternate screen, raw mode, cursor, and terminal state.
