import { defineConfig } from 'vitepress'

export default defineConfig({
  base: '/nono-approval/',
  lang: 'en-US',
  title: 'nono-approval',
  description: 'Documentation for the nono local approval daemon',

  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'Design', link: '/design/overview' },
      { text: 'GitHub', link: 'https://github.com/YewFence/nono-approval' }
    ],

    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started' }
        ]
      },
      {
        text: 'Design',
        items: [
          { text: 'Architecture Overview', link: '/design/overview' },
          { text: 'Domain Language', link: '/design/domain-language' },
          { text: 'Approval Lifecycle', link: '/design/approval-lifecycle' },
          { text: 'Protocol and Adaptation', link: '/design/protocol' },
          { text: 'Security Model', link: '/design/security' },
          { text: 'CLI and TUI', link: '/design/cli-and-tui' },
          { text: 'Operations, Configuration, and Releases', link: '/design/operations' },
          { text: 'Verification Status', link: '/design/testing' }
        ]
      },
      {
        text: 'Records',
        items: [
          { text: 'nono 0.69 Research', link: '/research/nono-0.69' },
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
