# nono-approval

`nono-approval` is a local approval daemon. It receives operations awaiting approval through the synchronous webhook ApprovalBackend of nono 0.69 and later, then moves the human interaction to a TUI or precise CLI commands in another terminal, so full-screen agent TUIs never contend with nono's terminal backend for the same TTY.

It does not execute commands, proxy passwords, modify the nono profile, or bypass nono's hard deny; every decision maps to exactly one full approval ID and takes effect exactly once.

## Platforms and installation

The MVP supports Linux and macOS:

```bash
cargo install nono-approval
```

You can also build from source:

```bash
mise run build
```

## Quick start

Create an owner-only config and print the nono profile snippet:

```bash
nono-approval setup
```

After merging the printed snippet into your final nono profile, start the daemon in the foreground:

```bash
nono-approval serve
```

Open another terminal for interactive approval:

```bash
nono-approval
```

In the TUI, `a` approves immediately, `d` denies immediately with a fixed reason, `D` denies with a custom reason, and `q` quits. Enter in browse mode never approves anything.

Scriptable control commands:

```bash
nono-approval status
nono-approval list --json
nono-approval show appr_0123456789abcdef
nono-approval approve appr_0123456789abcdef
nono-approval deny appr_0123456789abcdef --reason "outside this task"
```

`show`, `approve`, and `deny` accept only the full ID: `appr_` plus 16 lowercase hex characters. Prefixes, `latest`, and `all` are not supported.

## nono configuration essentials

Default webhook endpoint:

```text
http://127.0.0.1:17443/v1/webhooks/approval
```

The daemon's default Approval Lease is `270s`; the nono backend/default timeout emitted by `setup` is `300s`, leaving 30 seconds of headroom for delivering the explicit-denial HTTP response. If you change either side, keep the nono timeout greater than the daemon timeout.

The `0700` parent directory, `0600` file permissions, and peer UID checks on the Unix control socket cannot isolate a same-UID sandbox. Linux profiles should enable pathname AF_UNIX mediation, and macOS should use a restricted network mode that can confine Unix sockets. Once the daemon is running you can run a real probe:

```bash
nono-approval config validate --profile <name-or-path>
```

The probe passes only if the sandbox is confirmed started and the connection returns an explicit `EACCES` or `EPERM`. This command may additionally run the profile's host-side session hooks once.

## Configuration

`setup` creates `config.toml` in the platform-native config directory:

```toml
schema_version = 1

[webhook]
listen = "127.0.0.1:17443"

[approval]
request_timeout = "270s"
max_pending = 64
max_per_session = 8
max_body = "256KiB"
```

Unknown fields, a missing or unsupported schema version, a non-loopback listener, and insecure file permissions all cause failure. `serve` never implicitly creates, migrates, or rewrites the config. Runtime values resolve in the order: explicit CLI arguments, config file, built-in defaults.

## Debug Capture

Normal mode never writes approval details to disk or to ordinary logs; plaintext details are destroyed as soon as a request reaches a terminal state. Enable capture explicitly when you need diagnostics:

```bash
nono-approval serve --debug-capture
nono-approval debug captures
nono-approval debug clean
```

Every daemon start creates a dedicated `0600` NDJSON file in the owner-only state directory. Files never auto-rotate or expire; `debug clean` deletes only managed files that pass owner, type, and fixed-naming validation, and never deletes directories recursively.

## Shell completion

```bash
nono-approval completions bash
nono-approval completions zsh
nono-approval completions fish
```

## Development and verification

```bash
mise run test
mise run check
mise run dev
```

`mise run dev` starts the daemon (bottom pane) and the interactive TUI (top pane) together in a top/bottom split zellij session named `nono-dev`; running it again reattaches to the existing session.

See [`docs/design/overview.md`](docs/design/overview.md) for the architecture, security boundaries, protocol, and verification status.

## License

MIT
