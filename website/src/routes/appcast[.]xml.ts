import { createFileRoute } from '@tanstack/react-router'

const RELEASE_APPCAST_URL =
  'https://github.com/ZainCheung/pix/releases/latest/download/appcast.xml'

// Keep a valid empty feed available only while the latest GitHub release has
// not published an appcast yet (the upstream 404 bootstrap case). Other
// upstream failures are surfaced as HTTP errors so Sparkle can report that
// update checking failed.
const EMPTY_APPCAST = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Pix Updates</title>
    <link>https://pix.deepoke.com/</link>
    <description>Pix macOS application updates</description>
  </channel>
</rss>
`

export const Route = createFileRoute('/appcast.xml')({
  server: {
    handlers: {
      GET: async () => {
        const errorResponse = (status: number, message: string) =>
          new Response(message, {
            status,
            headers: {
              'Cache-Control': 'no-store',
              'Content-Type': 'text/plain; charset=UTF-8',
              'Retry-After': '300',
              'X-Content-Type-Options': 'nosniff',
            },
          })

        const abortController = new AbortController()
        const timeout = setTimeout(() => abortController.abort(), 8_000)
        try {
          const response = await fetch(RELEASE_APPCAST_URL, {
            headers: { Accept: 'application/xml, text/xml;q=0.9' },
            redirect: 'follow',
            signal: abortController.signal,
          })
          if (response.ok) {
            const body = await response.text()
            if (body.includes('<rss') && body.includes('<channel')) {
              return new Response(body, {
                headers: {
                  'Cache-Control': 'public, max-age=300',
                  'Content-Type': 'application/xml; charset=UTF-8',
                  'X-Content-Type-Options': 'nosniff',
                },
              })
            }
            return errorResponse(502, 'GitHub returned an invalid Pix appcast')
          }
          if (response.status === 404) {
            return new Response(EMPTY_APPCAST, {
              headers: {
                'Cache-Control': 'public, max-age=60',
                'Content-Type': 'application/xml; charset=UTF-8',
                'X-Content-Type-Options': 'nosniff',
              },
            })
          }
          return errorResponse(
            502,
            `GitHub appcast request failed with HTTP ${response.status}`,
          )
        } catch {
          return errorResponse(503, 'Pix update feed is temporarily unavailable')
        } finally {
          clearTimeout(timeout)
        }
      },
    },
  },
})
