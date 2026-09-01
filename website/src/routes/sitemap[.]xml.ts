import { createFileRoute } from '@tanstack/react-router'

import { SITEMAP_LASTMOD } from '#/generated/sitemap-lastmod'
import { siteUrl } from '#/lib/seo'
import { source } from '#/lib/source'
import { updateSource } from '#/lib/updates'

const ISO_DATE = /^\d{4}-\d{2}-\d{2}$/

const STATIC_PATHS = [
  '/',
  '/start',
  '/use-cases',
  '/use-cases/pi-from-iphone',
  '/use-cases/remote-pi',
  '/use-cases/continue-pi-sessions',
  '/use-cases/local-first-ai-coding',
  '/updates',
]

function escapeXml(value: string) {
  return value.replace(/[<>&'\"]/g, (character) => {
    switch (character) {
      case '<': return '&lt;'
      case '>': return '&gt;'
      case '&': return '&amp;'
      case "'": return '&apos;'
      case '"': return '&quot;'
      default: return character
    }
  })
}

function sitemapXml() {
  const paths = new Set([
    ...STATIC_PATHS,
    ...source.getPages().map((page) => page.url),
    ...updateSource.getPages().map((page) => page.url),
  ])
  const urls = [...paths]
    .sort()
    .map((path) => {
      const lastmod = SITEMAP_LASTMOD[path]
      if (!lastmod || !ISO_DATE.test(lastmod)) {
        throw new Error(`Missing sitemap lastmod metadata for ${path}`)
      }

      return [
        '  <url>',
        `    <loc>${escapeXml(siteUrl(path))}</loc>`,
        `    <lastmod>${escapeXml(lastmod)}</lastmod>`,
        '  </url>',
      ].join('\n')
    })
    .join('\n')

  return `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls}
</urlset>
`
}

export const Route = createFileRoute('/sitemap.xml')({
  server: {
    handlers: {
      GET: async () =>
        new Response(sitemapXml(), {
          headers: {
            'Cache-Control': 'public, max-age=3600',
            'Content-Type': 'application/xml; charset=UTF-8',
          },
        }),
    },
  },
})
