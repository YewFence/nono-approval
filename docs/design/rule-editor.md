# Rule Scope Selector

The approval TUI selects a literal path from the original request's ancestor chain. It has no path text input, Action radio field, focus cycle, submit button, or nested ancestor picker. This is a bounded approval interaction, not a general-purpose policy editor.

## Module responsibilities

| Module | Responsibility | Does not own |
| --- | --- | --- |
| `policy::RuleDraft` | Action, literal path, scope, exact access; normalization, compilation, ancestor choices, source-coverage validation | UI, approval IDs, lifetimes, storage |
| `rule_selector::RuleSelector` | Immutable ancestor chain, selected path, scope, safe rendering, action/cancel events | Source request identity, HTTP, file I/O, rule installation |
| `interactive::ApprovalRuleEditing` | Immutable source snapshot and ID, deadline/availability, daemon lifetime display, final submission | Matching implementation, reusable field editing |
| `Broker` | Authoritative pending/lease validation, inherited access, coverage check, atomic install and source decision | Terminal interaction |

`RuleSelector::new(draft)` validates the supplied path and captures its normalized self/ancestor chain once, without filesystem access. `handle_key` returns `Continue`, `Changed`, `Cancelled`, or `Submit(RuleDraft)`. The submit key determines Allow/Deny; opening or navigating never installs a rule. The host revalidates source coverage and availability before submitting to the daemon.

## Selection

`r` opens at the original request path with Exact path scope. `p` is a compatible Exact path entry point; `P` and `A` start with Tree scope. All four require a later explicit `a` or `d` decision. Ordinary queue `a`, `d`, and `D` retain their one-shot behavior.

| Key | Effect |
| --- | --- |
| Left / `h` | Shorten to the next parent, stopping at `/` |
| Right / `l` | Restore one component from the captured chain, stopping at the original request path |
| Space | Toggle Exact path/Tree at the original path only |
| `a` | Approve and remember the selected scope |
| `d` | Deny and remember the selected scope |
| Esc | Cancel and return to the queue |
| PageUp / PageDown | Scroll the selected path |
| Ctrl-Up / Ctrl-Down | Scroll source details |
| Ctrl-C | Exit the TUI |

Selecting an ancestor always uses Tree scope so the rule covers the source. Returning to the original path restores its previous scope. Space cannot create an exact ancestor rule, since that would not cover the request. The root Tree selection explicitly warns that it covers all absolute paths for the request's access mode. Navigation is reversible, does not wrap at either end, and never scans children or guesses a project root.

For `Read /path/to/project/src/main.rs`, pressing Left twice selects `/path/to/project` with Tree scope. `a` then approves the request and remembers that subtree for Read access only. Right twice returns to the original file and restores Exact path scope. Enter, Tab, text input, and explicit repeated action events cannot submit.

## Source and layout

Opening the selector retrieves the raw capability DTO through owner-authenticated `show?debug=true`. The source ID, original path/access/reason, and deadline remain fixed while the selection changes. A vanished source disables submission without retargeting the next queue item. Disconnect drops the selector. The daemon rechecks the monotonic lease and coverage under its lock; the displayed wall-clock countdown is informational.

At 90 columns or wider the source and selection are side by side. At 80x24 and in a 60-column split they stack vertically, with the selector taking priority. Each region has one border, with no nested frames. The selector shows the selected path once, its exact access and scope, and always-visible contextual controls. Paths wrap losslessly and can scroll independently of those controls. Below 40x22 the host displays a minimum-size message and disables selection/submission until resized; Esc and Ctrl-C remain available.

Paths stay literal. Original control characters and backslashes are visibly escaped only for display; the selected data never comes from sanitized text. No escape decoding, environment expansion, or glob interpretation occurs. Access remains inherited and locked.

## CLI counterpart

The CLI still allows an explicit `--rule-path` together with `--session-path` or `--session-dir`. Removing free-form TUI input does not remove this non-interactive API. Both paths use the same authoritative daemon coverage check:

```bash
nono-approval approve appr_0123456789abcdef --session-dir --rule-path /path/to/project
nono-approval approve appr_0123456789abcdef --session-path
```

There is no in-TUI policy editing or overwrite. Policy files live outside this selector: `nono-approval --policy <file>` loads a validated TOML batch before the TUI starts, and TUI `S` saves the daemon's current rules to a new file. The selector itself stays deliberately limited to a fixed source path and its ancestors.
