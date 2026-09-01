# Getting Started

## Installation

```bash
cargo install nono-approval
nono-approval setup
```

`setup` creates or verifies the owner-only `config.toml` in the platform-native project directory, then prints the current Webhook Endpoint and the nono approval backend snippet. Merge the snippet into your final nono profile; it never modifies the profile itself.

## Usage

Start the daemon in one terminal:

```bash
nono-approval serve
```

Run `nono-approval` in another terminal to open the TUI. When the daemon is not yet running, the TUI waits and reconnects every second. Press `a` to approve, `d` to deny quickly with a fixed reason, `D` to deny after entering a reason; Enter in browse mode never executes an approval decision.

## Configuration

The config is created by `setup` and must contain `schema_version = 1`. The default webhook is `127.0.0.1:17443`, the Approval Lease is `270s`, the pending limits are `64` globally and `8` per session, and the request body limit is `256KiB`. The control socket path is resolved from the platform project directory and is not written into this config file; use the hidden `--control-socket` argument when you need to override the path temporarily.

Run `nono-approval config validate --profile <name-or-path>` to check control socket isolation through a real nono sandbox; it passes only if the sandbox is confirmed started and the connection returns `EACCES` or `EPERM`. The command may additionally execute the target profile's host-side session hooks.

## Next steps

- [Architecture overview](../design/overview.md)
- [CLI and TUI](../design/cli-and-tui.md)
- [Operations, configuration, and releases](../design/operations.md)
- [Verification status](../design/testing.md)
