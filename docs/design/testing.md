# Verification Status

This document records what the current repository has already verified automatically, the CI platforms, and the risks that still require manual verification. It is not a plan for future implementation; functional semantics come from the corresponding topical docs and the code.

## Local entry points

The project exposes reusable checks through mise:

```bash
mise run test        # cargo test --workspace --all-targets --all-features --locked
mise run rust:check  # fmt, cargo check, clippy -D warnings
mise run repo:check  # lockfile, newline, typos, secret, and workflow checks
mise run docs:build  # VitePress build
mise run check       # repo:check, rust:check, test
```

## Current unit tests

### Broker and lifecycle

`src/broker.rs` currently covers:

- the full approval ID shape, rejecting a missing prefix, short IDs, and uppercase hex;
- empty, all-NUL, `4 KiB` boundary, and over-limit denial reasons;
- approve taking effect exactly once, with `show` returning the minimal Tombstone after completion;
- the monotonic deadline expiry returning a denial;
- duplicate, per-session, and global capacity.

### Wire Adapter and webhook

`src/protocol.rs` and `src/webhook.rs` currently cover:

- compatibility with extra unknown fields on command;
- unknown or incomplete variants;
- trailing JSON and the body limit;
- endpoint, capability, and network fixtures;
- invalid access/protocol and empty operation fields;
- the fixed webhook path.

The four fixtures pin the current DTO behavior of this project; the test dependency graph introduces no nono crate.

### Config and runtime path

`src/config.rs` currently covers:

- `setup` creating a `0600` file and being idempotent;
- unknown fields and insecure file permissions failing.

`src/runtime_path.rs` currently covers:

- owner-only directory creation and `0700` permissions;
- symlink path component rejection;
- existing permissive directories being rejected on read-only verification.

### Display and TUI

`src/display.rs` currently covers ANSI/OSC sanitization, visible escaping of C0 controls, and Unicode-width summary truncation.

`src/interactive.rs` currently covers:

- disconnects clearing client request state;
- basic wide/narrow rendering;
- the denial reason input mode never interpreting Enter as approve.

### Debug Capture

`src/debug_capture.rs` currently covers:

- creating, listing, and cleaning managed captures;
- rejection of unmanaged directory entries;
- received/completed NDJSON and safe sanitization of completion reasons;
- serialization of `response_delivery_outcome: not_observed`.

### CLI

`src/cli.rs` currently covers partial approval IDs failing at clap parse time, and every public subcommand having a help description.

## Current integration tests

`tests/bridge.rs` starts a real TCP webhook listener and a Unix control listener and covers:

1. a webhook request registering as pending;
2. control list finding the exact approval ID;
3. control approve returning granted;
4. the original webhook response receiving granted;
5. a second decision after terminal state returning conflict;
6. a control decision body over `8 KiB` returning `400` without deciding the request.

This integration test uses a temporary socket path and an in-process Broker; it does not start a full CLI process and does not run nono.

## CI

GitHub Actions currently runs:

- `mise run rust:check` on Linux and macOS;
- `mise run test` on Linux and macOS;
- Linux repository checks;
- the VitePress documentation build;
- periodic Rust dependency audit;
- four-target builds, archiving, crates.io publishing, and the GitHub Release on release.

Portable checks and packaging behavior are maintained by `mise.toml`, `mise.ci.toml`, and `scripts/package-release`; the workflow handles events, permissions, the platform matrix, artifacts, and release orchestration.

## Not yet verified automatically

The following behaviors exist in code or docs, but the current test suite provides no end-to-end evidence:

- real nono 0.69+ sending a webhook from a command policy and continuing or rejecting the operation after approve/deny;
- cross-terminal approval inside full-screen Pi, Claude Code, and Codex TUIs not contending for the original TTY;
- the reachable/denied matrix of `config validate` against real Linux Landlock and macOS Seatbelt profiles;
- Profile Validation timeouts, the invalid child protocol, the session hook disclosure, and each errno branch;
- different-UID socket-pair behavior and platform API failure paths of Linux/macOS peer identity;
- the full daemon's denial, 100ms flush, and socket cleanup after `SIGINT`/`SIGTERM`;
- network-level tests of the webhook's method, content type, all error status codes, and disconnect cancellation;
- Tombstone 1024-entry/10-minute eviction, replay TTL, and concurrent double-decision race tests;
- Debug Capture runtime I/O failure turning into failed while approvals continue;
- actual paths and socket ABI length boundaries for every ProjectDirs platform;
- all TUI keybindings, the 500ms/1s cadence, selection fallback, resize, and panic/abnormal terminal restore;
- the contents of the four release archives verified by unpacking in CI.

These items must not be phrased in release notes or user docs as acceptance criteria that already pass. When adding tests, verify the module's public interface directly and avoid building a parallel bypass implementation just for testing.

## Manual verification suggestions

When real nono or platform sandboxes are involved, record at least:

1. the exact nono version and profile;
2. the OS and architecture;
3. the daemon config and actual webhook/control addresses;
4. the approval ID, terminal state, and the original operation's outcome;
5. whether Debug Capture was enabled;
6. whether Profile Validation confirmed started, and the final errno/reachability result.

Research facts and manual experiments should be written into versioned research documents, never mixed into long-term protocol commitments.
