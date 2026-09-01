# nono 0.69 Webhook and Sandbox Research

This document records the versioned external facts the design depends on, and is not a long-term protocol commitment of nono-approval itself. Baseline version: `nono 0.69.0`, investigated 2026-07-27 through 2026-07-28.

## Approval Backend

nono already has several ApprovalBackends:

- terminal: reads interactive input from `/dev/tty`;
- webhook: makes a synchronous HTTP request and waits for the response;
- exec: invokes an external program;
- chain: combines backends with all/any.

This project uses the existing webhook and does not modify nono.

## TUI input contention

When full-screen agent TUIs such as Pi, Claude Code, and Codex occupy the current terminal, the terminal backend and the agent contend for the same TTY. In local verification on 2026-07-27, after Pi triggered the approve rule for `date`, nono showed the approval prompt, but both `y` presses went into the Pi PTY, and the approval eventually timed out and returned `approval_denied`.

Local approval interaction therefore must move to a separate terminal instead of contending for the agent's `/dev/tty`.

## Webhook Schema

Current outer structure:

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

Current variants: command, endpoint, capability, network. Accurate DTOs should be generated as fixtures from the local nono source and real CLI output, not handwritten guesses.

## Field provenance and trust

- `backend`: the backend name selected by the profile; the receiver can only treat it as a claimed backend;
- Command: `child_pid` comes from shim Unix socket peer credentials, `caller` from the host process relationship, `intercept_rule` from the profile; command identity is verified through shim fd/inode checks;
- Capability: explicit Supervisor IPC and seccomp-notify share a variant; some fields of the former may come from the child, and the wire has no origin discriminator;
- Endpoint: route/upstream/rule label come from the proxy/profile, method/path from proxy observation; the current child PID is `0` and the session ID is `proxy`;
- Network: types and tests exist in the source, but no production construction site was found at investigation time.

The webhook currently lacks:

- resolved executable path/identity;
- cwd;
- profile identity/digest;
- supervisor PID;
- session display name;
- agent entrypoint;
- a unified observed child PID;
- the final resolved deadline.

nono-approval must not guess these fields.

## Synchronous connection and cancellation defects

The webhook backend is a synchronous long-lived connection. The current request body carries no resolved timeout and there is no separate cancellation message.

Worse, nono's outer timeout may abandon a still-running backend, leaving the HTTP connection and backend alive after the original operation already failed closed. The receiver therefore cannot rely on connection drops to decide that a request is over; only a local deadline can serve as the sole Approval Lease.

Corresponding local upstream issue draft:

- `/home/yewfence/code/issues/nolabs-ai__nono___issue_3.md`

## Linux Pathname AF_UNIX

The official Landlock documentation explains that `linux.af_unix_mediation: "pathname"` being off by default is a compatibility design, not a least-privilege implementation bug. When enabled, pathname AF_UNIX becomes default-deny and needs explicit `filesystem.unix_socket*` grants.

Related source and docs:

- `../nono/docs/cli/internals/landlock.mdx`;
- `../nono/crates/nono/src/sandbox/linux.rs`.

A real probe proved: under the default profile, a same-UID sandbox can connect even when the directory is `0700` and the socket is `0600`; `/proc/net/unix` may also expose the random socket path. A random pathname is therefore not a secret and cannot replace AF_UNIX mediation.

## macOS Unix Socket

macOS Seatbelt restricted network mode allows connections according to explicit Unix socket grants; AllowAll does not provide the required isolation. When same-UID self-approval protection is needed, use a restricted mode such as Blocked or ProxyOnly and verify the real behavior of the final profile.

## Profile Introspection defects

`nono profile show --json` nominally outputs the fully resolved profile, but currently omits six kinds of `filesystem.unix_socket*` fields. The resolver does merge these fields, but the JSON output does not serialize them; the manifest also skips socket paths that did not exist at the time, so it cannot fully substitute for a resolved-profile API.

Corresponding local upstream issue draft:

- `/home/yewfence/code/issues/nolabs-ai__nono___issue_4.md`

The MVP's `config validate` therefore uses a real short-lived sandbox probe rather than static JSON validation alone.

## Session Hooks

`nono run --profile ...` executes the final profile's session hooks on the host side. An after hook cannot serve as automatic safety validation:

- it runs after the risk window;
- it runs on the host, not in the sandbox under test;
- a child profile can override the inherited hook;
- probing again with the same profile could recurse.

Real validation may additionally execute the user's hooks once — a known side effect the command must disclose.

## Related upstream issues

- [#47 interactive permission mode](https://github.com/nolabs-ai/nono/issues/47): early Allow once/Profile/Deny/Quit ideas;
- [#842 Grant permission on the fly](https://github.com/nolabs-ai/nono/issues/842): confirms the ApprovalBackend and external human approval direction;
- [#436 approval model with OPA/Rego](https://github.com/nolabs-ai/nono/issues/436) and [#879](https://github.com/nolabs-ai/nono/issues/879): external policy engines, not a local human queue;
- [#1198 Capability elevation freezes](https://github.com/nolabs-ai/nono/issues/1198): terminal approval freezing and garbling;
- [#1500 Route unmatched proxy destinations through ApprovalBackend](https://github.com/nolabs-ai/nono/issues/1500): a unified runtime decision entry point, but no local daemon provided.

As of the investigation date, no upstream implementation directly covering "local approval daemon + pending queue + interactive/list/show/approve/deny" was found.
