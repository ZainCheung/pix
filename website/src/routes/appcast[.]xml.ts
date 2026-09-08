import { createFileRoute } from '@tanstack/react-router'

const RELEASE_APPCAST_URL =
  'https://github.com/ZainCheung/pix/releases/latest/download/appcast.xml'

export const Route = createFileRoute('/appcast.xml')({
  server: {
    handlers: {
      GET: () =>
        new Response(null, {
          status: 302,
          headers: {
            Location: RELEASE_APPCAST_URL,
            'Cache-Control': 'no-store',
          },
        }),
    },
  },
})
