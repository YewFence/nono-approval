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

features:
  - title: 独立终端审批
    details: 避免 nono terminal backend 与 Pi、Claude Code、Codex 等全屏 TUI 争用同一个 TTY。
  - title: Fail closed
    details: 精确一次性决定、单调时钟 Lease、owner-only control socket，以及明确的容量和输出上限。
  - title: CLI 与 TUI
    details: 默认进入轮询式 TUI，同时提供 status、list、show、approve 和 deny 命令。
---
