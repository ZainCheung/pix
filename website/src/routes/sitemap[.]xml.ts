import { createFileRoute } from '@tanstack/react-router'

import { siteUrl } from '#/lib/seo'
import { source } from '#/lib/source'

const STATIC_PATHS = [
  '/',
  '/use-cases',
  '/use-cases/pi-from-iphone',
  '/use-cases/remote-pi',
  '/use-cases/continue-pi-sessions',
  '/use-cases/local-first-ai-coding',
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
  ])
  const urls = [...paths]
    .sort()
    .map((path) => `  <url><loc>${escapeXml(siteUrl(path))}</loc></url>`)
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
