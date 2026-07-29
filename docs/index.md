---
layout: home

hero:
  name: 'nono-approval'
  text: 'Local approval daemon for nono'
  tagline: '在独立终端安全地查看并决定一次性审批请求。'
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: Architecture
      link: /design/overview

features:
  - title: 独立终端审批
    details: 避免 nono terminal backend 与 Pi、Claude Code、Codex 等全屏 TUI 争用同一个 TTY。
  - title: Fail closed
    details: 精确一次性决定、单调时钟 Lease、owner-only control socket，以及明确的容量和输出上限。
  - title: CLI 与 TUI
    details: 默认进入轮询式 TUI，同时提供 status、list、show、approve 和 deny 命令。
---

## 当前实现

`nono-approval` 当前实现了 webhook bridge、内存 Broker、Unix-socket control
interface、精确 CLI、轮询式 TUI、Profile Validation probe 和显式 Debug Capture。

从[架构总览](/design/overview)开始，再按专题阅读[协议与适配](/design/protocol)、
[审批生命周期](/design/approval-lifecycle)和[安全模型](/design/security)。
