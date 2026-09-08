import { createFileRoute } from '@tanstack/react-router'

const RELEASE_APPCAST_URL =
  'https://github.com/ZainCheung/pix/releases/latest/download/appcast.xml'

// Keep a valid empty feed available before the first Sparkle-enabled release
// (and during a transient GitHub outage). Sparkle treats an empty feed as no
// available update and will retry on the next scheduled or manual check.
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
        try {
          const response = await fetch(RELEASE_APPCAST_URL, {
            headers: { Accept: 'application/xml, text/xml;q=0.9' },
            redirect: 'follow',
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
          }
        } catch {
          // Return the empty feed below. Sparkle will retry later and the
          // endpoint remains valid before the first release is published.
        }

        return new Response(EMPTY_APPCAST, {
          headers: {
            'Cache-Control': 'public, max-age=60',
            'Content-Type': 'application/xml; charset=UTF-8',
            'X-Content-Type-Options': 'nosniff',
          },
        })
      },
    },
  },
})
