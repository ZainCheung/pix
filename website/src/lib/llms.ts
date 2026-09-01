import { GITHUB_URL, HOME_DESCRIPTION, IOS_APP_URL, siteUrl } from '#/lib/seo'
import { source } from '#/lib/source'
import { USE_CASES } from '#/components/use-case-page'

const FALLBACK_DOC_DESCRIPTION =
  'Documentation for installing, using, and developing Pix with Pi.'

function oneLine(value: string) {
  return value.replace(/\s+/g, ' ').trim()
}

function docsLinks() {
  return source
    .getPages()
    .map((page) => {
      const title = typeof page.data.title === 'string'
        ? page.data.title
        : 'Pix documentation'
      const description = typeof page.data.description === 'string'
        ? page.data.description
        : FALLBACK_DOC_DESCRIPTION

      return {
        url: siteUrl(page.url),
        title: oneLine(title),
        description: oneLine(description),
      }
    })
    .sort((a, b) => a.url.localeCompare(b.url))
}

function useCaseLinks() {
  return Object.values(USE_CASES)
    .map((page) => ({
      url: siteUrl(`/use-cases/${page.slug}`),
      title: oneLine(page.h1),
      description: oneLine(page.description),
    }))
    .sort((a, b) => a.url.localeCompare(b.url))
}

/**
 * Generate the AI-readable site index from the same sources as the website.
 * Keep this concise: the linked pages remain the source of truth for details.
 */
export function llmsTxt() {
  const lines = [
    '# Pix',
    '',
    `> ${HOME_DESCRIPTION}`,
    '',
    'Pix is an open-source, local-first iPhone client for the Pi coding agent.',
    'Pi runs on your Mac or Linux machine while Pix lets you start, resume, and control native Pi sessions remotely.',
    '',
    '## Product',
    `- [Homepage](${siteUrl('/')}) — Product overview and installation options.`,
    `- [Download Pix for iPhone](${IOS_APP_URL}) — iOS app download.`,
    `- [Use cases](${siteUrl('/use-cases')}) — Remote Pi workflows from an iPhone.`,
    `- [GitHub repository](${GITHUB_URL}) — Source code and contribution history.`,
    '',
    '## Scope and constraints',
    '- Pi is the only supported agent runtime.',
    '- Pi, your repository, credentials, tools, and native session files stay on the Mac or Linux host.',
    '- Workspaces must be explicitly authorized, and clients must be paired with the host.',
    '- Pix connects directly over the local network when possible and uses an encrypted relay when devices are on different networks.',
    '- The relay forwards opaque encrypted frames; it does not run Pi or store application payloads.',
    '',
    '## Use cases',
    ...useCaseLinks().map(({ url, title, description }) => `- [${title}](${url}) — ${description}`),
    '',
    '## Documentation',
    ...docsLinks().map(({ url, title, description }) => `- [${title}](${url}) — ${description}`),
    '',
    '## Source',
    `- [Documentation source](${GITHUB_URL}/tree/main/docs) — Markdown source for the Pix documentation.`,
    `- [Website source](${GITHUB_URL}/tree/main/website) — Website routes and presentation.`,
    '',
  ]

  return `${lines.join('\n')}\n`
}
