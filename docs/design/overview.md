# Architecture Overview

`nono-approval` is a local approval daemon. It receives operations awaiting approval through nono's synchronous webhook ApprovalBackend, then moves the human interaction to a TUI or precise CLI commands in another terminal, so full-screen agent TUIs never contend with nono's terminal backend for the same TTY.

This document only describes product boundaries, modules, and their interfaces. Protocol fields, state transitions, security constraints, interaction behavior, platform paths, and verification evidence are maintained by the corresponding topical docs.

## Product boundaries

`nono-approval` only returns a user's one-shot decision for exactly one Approval Request:

```text
Approve exactly this request once
Deny exactly this request once
```

The daemon does not execute the operation in a request, does not proxy sudo or passwords, does not modify the nono profile, and does not override nono's hard deny, protected roots, or platform sandbox constraints.

The current implementation supports:

- the synchronous webhook ApprovalBackend of nono `0.69` and later;
- the four wire variants: command, endpoint, capability, and network;
- an HTTP control interface over an owner-only Unix control socket;
- precise-ID `status/list/show/approve/deny` CLI;
- a polling-based full-screen TUI started when no subcommand is given;
- platform paths and peer identity adapters for Linux and macOS;
- explicit, owner-only NDJSON Debug Capture.

Not supported:

- session-level or permanent approval;
- automatically judging whether an operation is safe, auto-approving, or generating policy;
- recovering pending requests after a daemon restart;
- cross-host approval, a browser UI, or a general policy engine;
- using ordinary logs or Tombstones as an authoritative audit log;
- distinguishing the host user from a sandboxed agent when same-UID processes can already access the control socket.

## Overall structure

```text
┌────────────────── nono ──────────────────┐
│ Tool Sandbox / proxy / supervisor        │
│ ApprovalBackend::request_approval        │
└───────────────────┬──────────────────────┘
                    │ loopback HTTP webhook
                    ▼
┌────────────── nono-approval serve ──────────────┐
│ Wire Adapter -> Broker/PendingStore             │
│        ├── Approval Lease / Tombstone           │
│        ├── Debug Capture (explicitly enabled)   │
│        └── HTTP over owner-only Unix socket     │
└───────────────────┬─────────────────────────────┘
                    ▼
            CLI / interactive TUI
```

The production dependency graph contains no nono crate. The Wire Adapter defines the currently compatible DTOs within this project; the real nono is the external wire source of truth, not a runtime library dependency.

## Modules and interfaces

### Wire Adapter

The Wire Adapter is the compatibility seam between webhook JSON and locally known requests. It is responsible for size limits, known-variant parsing, required-field validation, and constructing the terminal-safe display model. Unknown variants or requests that cannot be fully displayed fail closed.

See [Protocol and adaptation](protocol.md).

### Broker

The Broker is the core module of the approval lifecycle. It registers requests, assigns approval IDs, enforces capacity and replay limits, maintains the daemon's monotonic-clock Approval Lease, and delivers each request's one-shot decision back to the corresponding webhook handler.

Callers only touch it through the submit, list, show, decide, and shutdown behaviors; pending details, oneshots, Tombstones, and the replay cache are all encapsulated inside the module.

See [Approval lifecycle](approval-lifecycle.md).

### Webhook and Control adapters

The Webhook adapter only receives nono requests on loopback TCP. The Control adapter serves an HTTP interface to the CLI, TUI, and Profile Validation probe over a Unix socket. The two entry points share the Broker but share no listen address or authentication model.

See [Protocol and adaptation](protocol.md) and [Security model](security.md).

### CLI and TUI

The CLI and TUI are both clients of the control interface and never access internal Broker state directly. Decisions always carry the full approval ID, so a queue refresh, concurrent clients, or a vanished request can never transfer an action onto another request.

See [CLI and TUI](cli-and-tui.md).

### Platform adapter

Platform differences are confined to native project directory resolution and control peer identity: Linux uses `SO_PEERCRED`, macOS uses `LOCAL_PEERPID` with `getpeereid`. The rest of the Broker, protocol, display, and interaction logic stays platform-independent.

See [Operations, configuration, and releases](operations.md).

## Document map

- [Domain language](domain-language.md): Approval Lease, Tombstone, Wire Adapter, and other terms;
- [Approval lifecycle](approval-lifecycle.md): Broker, deadline, Tombstone, replay, and shutdown;
- [Protocol and adaptation](protocol.md): webhook schema, HTTP status codes, and the control interface;
- [Security model](security.md): trust model, Unix socket, Profile Validation, logging, and Debug Capture;
- [CLI and TUI](cli-and-tui.md): commands, output, keybindings, polling, and layout;
- [Operations, configuration, and releases](operations.md): platform paths, config schema, service examples, and release artifacts;
- [Verification status](testing.md): current automated coverage, CI, and behavior that still requires manual verification;
- [nono 0.69 research](../research/nono-0.69.md): versioned external facts the current design depends on;
- [ADR](../adr/0001-daemon-deadline-defines-approval-lease.md): cross-module trade-offs that have already converged.

## Implementation status

All modules above already have implementations. Topical docs describe current code behavior; anything not yet verified automatically is stated explicitly in [Verification status](testing.md) and never presented as a completed guarantee.
