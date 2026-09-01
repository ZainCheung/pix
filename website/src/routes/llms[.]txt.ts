import { createFileRoute } from '@tanstack/react-router'

import { llmsTxt } from '#/lib/llms'

export const Route = createFileRoute('/llms.txt')({
  server: {
    handlers: {
      GET: async () =>
        new Response(llmsTxt(), {
          headers: {
            'Cache-Control': 'public, max-age=3600',
            'Content-Type': 'text/plain; charset=UTF-8',
            'X-Content-Type-Options': 'nosniff',
          },
        }),
    },
  },
})
