import { loader } from 'fumadocs-core/source'
import { defineDocs } from 'fumadocs-mdx/macro'

export const docs = defineDocs({
  dir: '../docs',
  docs: {
    files: [
      'index.md',
      'QUICKSTART.md',
      'INSTALLATION.md',
      'PAIRING.md',
      'SESSIONS.md',
      'WORKSPACES.md',
      'REMOTE_ACCESS.md',
      'PI_TUI_BRIDGE.md',
      'DEVICES.md',
      'TROUBLESHOOTING.md',
      'HOW_PIX_WORKS.md',
      'LOCAL_FIRST.md',
      'SESSION_OWNERSHIP.md',
      'PAIRING_AND_TRUST.md',
      'DIRECT_VS_RELAY.md',
      'PIX_AND_PI.md',
      'SECURITY.md',
      'LIMITATIONS.md',
      'CLI.md',
      'ARCHITECTURE.md',
      'DEVELOPMENT.md',
      'REPOSITORY.md',
      'RELEASE.md',
      'PI_RPC_COVERAGE.md',
      'INSTALLATION_DETAILS.md',
      'SELF_HOST_RELAY.md',
      'TUI_BRIDGE_INTERNALS.md',
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
