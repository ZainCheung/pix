import { GITHUB_URL, HOME_DESCRIPTION, IOS_APP_URL, siteUrl } from '#/lib/seo'
import { source } from '#/lib/source'
import { updateSource } from '#/lib/updates'
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

function updateLinks() {
  return updateSource
    .getPages()
    .map((page) => ({
      url: siteUrl(page.url),
      title: oneLine(page.data.title),
      description: oneLine(page.data.description ?? 'Pix product update.'),
      date: page.data.date,
      releaseStatus: page.data.releaseStatus,
      order: page.data.order,
    }))
    .sort((a, b) =>
      b.date.localeCompare(a.date) ||
      a.order - b.order ||
      a.url.localeCompare(b.url),
    )
}

/**
 * Generate the AI-readable site index from the same sources as the website.
 * Keep this concise: the linked pages remain the source of truth for details.
 */
export function llmsTxt() {
  const updates = updateLinks()
  const releasedUpdates = updates.filter((update) => update.releaseStatus === 'published')
  const previewUpdates = updates.filter((update) => update.releaseStatus === 'preview')
  const updateSections: string[] = []

  if (releasedUpdates.length > 0) {
    updateSections.push(
      '## Updates',
      ...releasedUpdates.map(({ url, title, description, date }) =>
        `- [${title}](${url}) — ${date}: ${description}`,
      ),
      '',
    )
  }

  if (previewUpdates.length > 0) {
    updateSections.push(
      '## Coming next',
      ...previewUpdates.map(({ url, title, description }) =>
        `- [${title}](${url}) — Available on main: ${description}`,
      ),
      '',
    )
  }

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
    `- [Updates](${siteUrl('/updates')}) — First-party notes about new Pix product capabilities.`,
    `- [GitHub repository](${GITHUB_URL}) — Source code and contribution history.`,
    '',
    '## Scope and constraints',
    '- Pi is the only supported agent runtime.',
    '- Pix Host keeps Pi, your repository, credentials, tools, and native session files on the Mac or Linux host.',
    '- Workspaces must be explicitly authorized, and clients must be paired with the host.',
    '- Pix connects directly over the local network when possible and uses an encrypted relay when devices are on different networks.',
    '- The relay forwards opaque encrypted frames; it does not run Pi or store application payloads.',
    '- Pi may send relevant context to the model provider configured by the user; that provider policy is separate from Pix Relay.',
    '',
    '## Use cases',
    ...useCaseLinks().map(({ url, title, description }) => `- [${title}](${url}) — ${description}`),
    '',
    ...updateSections,
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
