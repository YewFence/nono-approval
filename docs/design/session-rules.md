# Session Rules

Session Rules are explicit runtime shortcuts for repeated capability requests. They live only in the approval daemon's memory, apply to every nono session using that daemon, and disappear on clear, shutdown, or restart. "Session" here means the daemon run, not a nono `session_id`, TUI connection, or terminal lifetime.

The default TUI client can load a complete rule set with `nono-approval --policy <name-or-path>`. Bare names resolve to `~/.config/nono-approval/<name>.toml`; explicit relative and absolute paths are also supported. The file uses `[[rules]]` entries with `action`, `path`, `scope`, and `access`. The client validates the complete file before one atomic `PUT /v1/session-rules` request. Duplicate keys are rejected, and an empty list clears all rules.

## Creating a rule

Select a pending capability request and press `r` to open the [Rule Scope Selector](rule-editor.md) at its original path with Exact path scope. Left/`h` selects a parent with Tree scope; Right/`l` restores one component, stopping at the original path. Space toggles Exact path/Tree at the original path only. `p` also opens Exact path; `P` and `A` open Tree. No entry point decides immediately: `a` approves and remembers, `d` denies and remembers, and Esc cancels. Ordinary queue `a`, `d`, and `D` retain their one-shot behavior. Immediate CLI equivalents are:

```bash
nono-approval deny appr_0123456789abcdef --session-path
nono-approval deny appr_0123456789abcdef --session-dir
nono-approval approve appr_0123456789abcdef --session-dir
nono-approval approve appr_0123456789abcdef --session-path
nono-approval approve appr_0123456789abcdef --session-dir --rule-path /path/to/project
nono-approval status
nono-approval session-rules clear
```

`deny` accepts `--reason` with either rule scope; the reason applies to the source request, not future automatic denials. Both approve and deny expose mutually exclusive scope flags. Optional `--rule-path` requires a scope flag. Exact-path remembered approval affects future requests and is not equivalent to a one-shot approval.

The daemon inherits the exact access mode from the selected pending request: remembering `Read` never matches `Write` or `ReadWrite`. The TUI restricts selection to the source path and ancestors; the CLI still accepts an explicit `--rule-path`. The daemon validates that the final path/scope covers the source and never accepts a client access override. Non-capability requests, expired/completed IDs, invalid or unrelated paths, and capacity failures cannot install rules or decide another request. The TUI uses the known raw DTO for selection, not sanitized display fields.

## Path scope

| Source path | Scope | Matches | Does not match |
| --- | --- | --- | --- |
| `/work/nono` | `path` | `/work/nono` | `/work/nono/file` |
| `/work/nono` | `directory` | `/work/nono`, `/work/nono/file`, `/work/nono/sub/file` | `/work`, `/work/nono-other` |

Directory scope means the chosen rule path itself and all component-boundary descendants. It never automatically chooses the parent directory. In the TUI, explicitly navigating to an ancestor selects Tree scope because an exact ancestor cannot cover the source. For example, `/path/to/project` with Tree covers `/path/to/project/src/main.rs`; the same ancestor with Exact path does not. The wire request does not tell us whether a path is a file or a directory, so scope is a lexical operation, not a filesystem type assertion. The daemon does not stat paths, follow symlinks, canonicalize through the filesystem, or scan directories. Allow rules trust nono's requested path and do not provide inode-based confinement.

Matching is case-sensitive. Repeated separators, `.` and trailing separators are normalized using path components; relative paths, parent components (`..`), NULs, and paths longer than 4096 bytes cannot form or match a rule. An incoming request with such a path continues through ordinary approval if it passes the existing wire checks. Wildcards and environment-variable syntax in actual filenames remain literal. A remembered `/work/*` never becomes a wildcard grant.

## Precedence and limits

The longest literal path wins; at the same path, exact scope wins over directory scope. This lets an exact denial override a broader allow, or a deeper directory allow override a broader denial. Rules with different access modes are independent. Re-adding the same normalized path, scope, and access replaces its action with the latest choice. Replacement is possible even at the 128-rule limit; a new distinct rule at capacity fails without deciding its source request.

The remember operation and source-request decision are atomic under the Broker lock, including validation of edited path coverage. Rule evaluation and unmatched registration also share that lock. Other requests already pending remain pending, even when a new rule would match them. Changing an existing rule therefore requires another suitable pending request or clearing rules first; this version has no per-rule management UI or individual removal. The scope selector always binds to a pending source, while daemon-wide clearing is available independently through the CLI and TUI.

`status` and the TUI footer report `session_rule_count`. `session-rules clear`, or TUI browse `C` followed by `y`, removes every allow and deny rule in this daemon, including rules created through other TUI/CLI clients. The TUI displays this daemon-wide scope before confirmation; `n` or Esc cancels, and Enter never confirms. The entry point is also available with an empty queue. Clearing leaves pending requests and their leases untouched. Closing the TUI does not remove rules.

## Automatic decisions

After HTTP, body, wire, and display validation, a matching rule returns `200 granted` or `200 denied` immediately. Hits create no approval ID, pending entry, Tombstone, lease, or replay entry and do not consume pending capacity. They also bypass existing replay checks. This shortcut cannot override nono hard denies or authorize operations the daemon never receives. Shutdown disables rule evaluation before denying pending requests.

Future denial reasons identify the normalized rule path and scope, for example `denied by session rule "/work" (Directory)`, with terminal-safe path escaping. Source requests retain the ordinary completion record with `decision_source: control_session_rule`.

Rule hits produce an info log containing action, scope, access, the sanitized rule path, and a short session ID. This is an explicit exception to the ordinary-log no-path policy. When Debug Capture is enabled, a `policy_decision` event also records the known Wire DTO, matched rule, decision, and `response_delivery_outcome: not_observed`; it has no approval ID. Diagnostics do not prove nono received the response.

## Future configuration

This version has no `policy.toml`, policy-file environment variable, policy-file CLI flag, persistence, or hot reload. The [Rule Scope Selector](rule-editor.md) has no free-form editing or file lifecycle; a future policy editor may reuse draft validation but needs its own UI. The component matcher supports exact paths, trailing `/` (self plus descendants), `*` (one component), and `**` (one or more components), but runtime clients submit literal exact/subtree rules only. Component-internal `*` patterns are rejected by the glob parser; literal rules do not interpret them as glob source. Middle `**` also consumes at least one component: `/a/**/test` does not match `/a/test`. Persistent rule loading and precedence between persistent and runtime rules remain separate future work.
