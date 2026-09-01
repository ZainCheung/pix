import { loader } from 'fumadocs-core/source'
import { defineDocs } from 'fumadocs-mdx/macro'

export const docs = defineDocs({
  dir: '../docs',
  docs: {
    files: [
      'index.md',
      'INSTALLATION.md',
      'REMOTE_ACCESS.md',
      'CLI.md',
      'PI_TUI_BRIDGE.md',
      'TROUBLESHOOTING.md',
      'ARCHITECTURE.md',
      'DEVELOPMENT.md',
      'REPOSITORY.md',
      'RELEASE.md',
      'PI_RPC_COVERAGE.md',
    ],
    async: true,
    postprocess: {
      includeProcessedMarkdown: true,
    },
  },
  meta: {
    files: ['meta.json'],
  },
})

function normalizeSlug(slug: string) {
  return slug.toLowerCase().replaceAll('_', '-')
}

export const source = loader({
  baseUrl: '/docs',
  source: docs.toFumadocsSource(),
  slugs: (_file, next) => next().map(normalizeSlug),
})
