import { defineConfig } from 'vitepress'

export default defineConfig({
  base: '/{{REPO_NAME}}/',
  lang: 'en-US',
  title: '{{PROJECT_NAME}}',
  description: '{{PROJECT_DESCRIPTION}}',

  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'GitHub', link: 'https://github.com/{{GITHUB_OWNER}}/{{REPO_NAME}}' }
    ],

    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started' }
        ]
      }
    ],

    search: {
      provider: 'local'
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/{{GITHUB_OWNER}}/{{REPO_NAME}}' }
    ],

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright © {{GITHUB_OWNER}}'
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
