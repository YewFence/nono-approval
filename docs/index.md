---
layout: home

hero:
  name: 'nono-approval'
  text: 'Local approval daemon for nono'
  tagline: 'View and decide one-shot approval requests safely in a separate terminal.'
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: Architecture
      link: /design/overview

features:
  - title: Approval in a separate terminal
    details: Avoid contending for the same TTY between nono's terminal backend and full-screen TUIs such as Pi, Claude Code, and Codex.
  - title: Fail closed
    details: Precise one-shot decisions, a monotonic-clock Lease, an owner-only control socket, and explicit capacity and output limits.
  - title: CLI and TUI
    details: Polling-based TUI by default, plus status, list, show, approve, and deny commands.
---

## Current implementation

`nono-approval` currently implements the webhook bridge, in-memory Broker, Unix-socket control interface, precise CLI, polling-based TUI, Profile Validation probe, and explicit Debug Capture.

Start with the [architecture overview](/design/overview), then read the topical docs on [protocol and adaptation](/design/protocol), [approval lifecycle](/design/approval-lifecycle), and the [security model](/design/security).
