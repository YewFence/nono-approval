# Operations, Configuration, and Releases

This document describes the current implementation's project paths, config loading, service operation, platform adapters, repository layout, and release artifacts. For security details see [Security model](security.md).

## Platform scope

The current code implements control peer identity on Linux and macOS:

- Linux: `SO_PEERCRED` from `nix`;
- macOS: `LOCAL_PEERPID` with `getpeereid` from `nix`;
- other platforms: the peer identity adapter returns unsupported, so a control service cannot be provided safely.

The production crate globally sets `unsafe_code = "forbid"` and never calls `libc` directly. The Broker, protocol, display, and TUI contain no platform branches.

## Project paths

`ProjectPaths::resolve()` uses:

```rust
ProjectDirs::from("dev", "YewFence", "nono-approval")
```

and resolves these paths:

| Purpose | Resolution |
| --- | --- |
| config | `ProjectDirs.config_dir()/config.toml` |
| state | `ProjectDirs.state_dir()`; falls back to `data_local_dir()` when there is no state dir |
| runtime | `ProjectDirs.runtime_dir()`; falls back to `data_local_dir()/runtime` when there is no runtime dir |
| control | `runtime/control.sock` |

In practice Linux follows XDG and macOS uses the native user directories provided by `ProjectDirs`. The docs treat neither `$XDG_CONFIG_HOME` nor `$XDG_RUNTIME_DIR` as a cross-platform interface, and never hardcode macOS absolute paths.

The control socket path must fit the platform `sockaddr_un.sun_path` length limit: Linux is currently checked against `107` bytes, macOS against `103` bytes. An over-long path fails startup; it never falls back to a shared `/tmp`.

## Config file

The config is a strict TOML schema:

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

`ConfigFile` currently has only three field groups: `schema_version`, `webhook`, and `approval`; the control socket is not written to the config and comes from the platform default path or the CLI `--control-socket`. Unknown fields, a missing version, a non-integer version, invalid TOML, a non-loopback listener, zero limits, and `max_per_session > max_pending` all fail.

The file must be a regular file owned by the current user, must not be a symlink, and must have exactly `0600` permissions. `setup` writes atomically on first creation; `load` and `serve` only read, never migrating or repairing the file.

Runtime values are overridden in this order:

```text
explicit CLI arguments > config.toml > built-in defaults
```

CLI overrides go through the same loopback, positive-number, and capacity-relation validation.

## Foreground daemon

Currently the daemon runs directly in the foreground:

```bash
nono-approval serve
```

The CLI never daemonizes by itself. Users can put it in tmux, their own process manager, or the service examples provided in the repo. On `SIGINT`/`SIGTERM` the Broker turns pending requests into denials, waits `100ms`, then stops the server tasks and removes the socket.

## Service examples

The repository currently provides:

- `examples/systemd/nono-approval.service`;
- `examples/launchd/dev.yewfence.nono-approval.plist`.

They are examples for users to adjust paths, environment, and startup arguments. The CLI never installs, enables, disables, or modifies them.

## nono profile integration

The minimal snippet printed by `setup` contains only the webhook backend and approval defaults:

```json
{
  "command_policies": {
    "approval_backends": {
      "local-broker": {
        "type": "webhook",
        "url": "http://127.0.0.1:17443/v1/webhooks/approval",
        "timeout_secs": 300
      }
    },
    "approval_defaults": {
      "backend": "local-broker",
      "timeout_secs": 300
    }
  }
}
```

If same-UID sandbox access to the control socket must be isolated, the user still needs to configure the platform-appropriate Unix socket/network mediation in the final profile; `setup` neither generates nor enforces a profile. Actual behavior is verified with `nono-approval config validate --profile ...`, see [Security model](security.md#profile-validation).

## Code layout

The main implementation files of the current repository:

```text
src/
├── main.rs                 # process entry
├── lib.rs                  # crate exports and version
├── cli.rs                  # clap commands and exit paths
├── daemon.rs               # listeners, tasks, and shutdown
├── webhook.rs              # loopback HTTP ingress
├── control.rs              # Unix-socket HTTP control
├── broker.rs               # pending, decision, Lease, Tombstone
├── protocol.rs             # Wire Adapter DTOs and parsing
├── display.rs              # safe sanitization, summary, and detail
├── interactive.rs          # ratatui TUI
├── config.rs               # TOML schema and atomic setup
├── runtime_path.rs         # ProjectDirs and owner-only paths
├── peer_identity.rs        # Linux/macOS peer UID
├── profile_validation.rs   # nono sandbox probe
└── debug_capture.rs        # NDJSON capture
tests/bridge.rs             # webhook/control bridge integration test
```

The main seams between modules are `Broker`, `ControlClient`, `KnownApprovalRequest`, `ProjectPaths`, and `DebugCapture`; there is no database, web UI, plugin system, or policy engine.

## Dependencies and tooling

Core dependencies grouped by responsibility:

- Tokio: async runtime, sockets, signals, timeouts, oneshots;
- Hyper/hyper-util/http-body-util: webhook and control HTTP;
- Clap/clap_complete: CLI and completion;
- Ratatui/crossterm: TUI;
- directories: platform project directories;
- serde/serde_json/toml: wire, control, and config DTOs;
- nix: safe wrappers for Linux/macOS peer credentials;
- vte, shlex, unicode-width, textwrap: terminal sanitization, display quoting, and layout;
- blake3, getrandom, tempfile, jiff: process-scoped hashing, randomness, atomic files, and time handling.

Production dependencies contain no nono crate.

## Release status

The repository already has crates.io and GitHub Releases publishing tasks. The GitHub Release builds four targets:

```text
x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu
x86_64-apple-darwin
aarch64-apple-darwin
```

Each archive is generated by `scripts/package-release` as a `.tar.gz` containing:

- the `nono-approval` binary;
- `README.md`;
- `LICENSE`;
- the systemd service example on Linux, or the launchd agent example on macOS.

The current release chain has no musl, deb/rpm, Homebrew tap, or curl install script.

## Local development entry points

The project organizes reusable tasks with mise:

```bash
mise run check       # repository checks, formatting, build, lint, tests
mise run test        # Rust tests
mise run docs:build  # VitePress documentation build
mise run build       # release build
```

The CI workflow only handles platform and GitHub Actions orchestration; the actual portable checks come from the `mise.toml` and `mise.ci.toml` tasks.
