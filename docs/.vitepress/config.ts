import { defineConfig } from 'vitepress'

export default defineConfig({
  base: '/nono-approval/',
  lang: 'zh-CN',
  title: 'nono-approval',
  description: 'nono 本地审批守护进程文档',

  themeConfig: {
    nav: [
      { text: '指南', link: '/guide/getting-started' },
      { text: '设计', link: '/design/overview' },
      { text: 'GitHub', link: 'https://github.com/YewFence/nono-approval' }
    ],

    sidebar: [
      {
        text: '指南',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started' }
        ]
      },
      {
        text: '设计',
        items: [
          { text: '架构总览', link: '/design/overview' },
          { text: '领域语言', link: '/design/domain-language' },
          { text: '审批生命周期', link: '/design/approval-lifecycle' },
          { text: '协议与适配', link: '/design/protocol' },
          { text: '安全模型', link: '/design/security' },
          { text: 'CLI 与 TUI', link: '/design/cli-and-tui' },
          { text: '运行、配置与发布', link: '/design/operations' },
          { text: '验证现状', link: '/design/testing' }
        ]
      },
      {
        text: '记录',
        items: [
          { text: 'nono 0.69 调研', link: '/research/nono-0.69' },
          { text: 'ADR 0001', link: '/adr/0001-daemon-deadline-defines-approval-lease' },
          { text: 'ADR 0002', link: '/adr/0002-support-linux-and-macos-through-local-platform-adapters' },
          { text: 'ADR 0003', link: '/adr/0003-leave-same-uid-control-isolation-to-deployment' }
        ]
      }
    ],

    search: {
      provider: 'local'
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/YewFence/nono-approval' }
    ],

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright © YewFence'
    },

    docFooter: {
      prev: 'Previous page',
      next: 'Next page'
    },

    outline: {
      label: 'On this page'
    },

    lastUpdated: {
      text: 'Last updated'
    }
  }
})
