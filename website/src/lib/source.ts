import { loader } from 'fumadocs-core/source'
import { defineDocs } from 'fumadocs-mdx/macro'

export const docs = defineDocs({
  dir: '../docs',
  docs: {
    files: [
      'index.mdx',
      '(start)/QUICKSTART.md',
      '(start)/INSTALLATION.md',
      '(start)/PAIRING.md',
      '(use-pix)/SESSIONS.md',
      '(use-pix)/WORKSPACES.md',
      '(use-pix)/REMOTE_ACCESS.md',
      '(use-pix)/PI_TUI_BRIDGE.md',
      '(use-pix)/DEVICES.md',
      '(use-pix)/TROUBLESHOOTING.md',
      '(understand-pix)/HOW_PIX_WORKS.md',
      '(understand-pix)/LOCAL_FIRST.md',
      '(understand-pix)/SESSION_OWNERSHIP.md',
      '(understand-pix)/PAIRING_AND_TRUST.md',
      '(understand-pix)/DIRECT_VS_RELAY.md',
      '(understand-pix)/PIX_AND_PI.md',
      '(understand-pix)/SECURITY.md',
      '(understand-pix)/LIMITATIONS.md',
      '(reference)/CLI.md',
      '(develop-pix)/ARCHITECTURE.md',
      '(develop-pix)/DEVELOPMENT.md',
      '(develop-pix)/REPOSITORY.md',
      '(develop-pix)/RELEASE.md',
      '(develop-pix)/PI_RPC_COVERAGE.md',
      '(advanced)/INSTALLATION_DETAILS.md',
      '(advanced)/SELF_HOST_RELAY.md',
      '(develop-pix)/TUI_BRIDGE_INTERNALS.md',
    ],
    async: true,
    postprocess: {
      includeProcessedMarkdown: true,
    },
  },
  meta: {
    files: ['**/meta.json'],
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
