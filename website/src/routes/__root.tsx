import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { HeadContent, Scripts, createRootRoute } from '@tanstack/react-router'
import { RootProvider } from 'fumadocs-ui/provider/tanstack'
import { useState, type ReactNode } from 'react'

import { THEME_BOOTSTRAP_SCRIPT } from '#/lib/theme'
import appCss from '#/styles.css?url'

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1, viewport-fit=cover' },
      { title: 'Pix · Your Pi stays on your machine' },
      {
        name: 'description',
        content:
          'Pix is an open-source, local-first remote host for Pi. Keep your code and sessions on your machine, then control them from anywhere.',
      },
      { name: 'theme-color', content: '#ffffff' },
      { property: 'og:title', content: 'Pix · Your Pi stays on your machine' },
      {
        property: 'og:description',
        content: 'Secure remote control for the Pi already running on your machine.',
      },
      { property: 'og:type', content: 'website' },
      { property: 'og:url', content: 'https://pix.deepoke.com/' },
      { property: 'og:image', content: 'https://pix.deepoke.com/og-image.png' },
      { name: 'twitter:card', content: 'summary_large_image' },
      { name: 'twitter:title', content: 'Pix · Your Pi stays on your machine' },
      {
        name: 'twitter:description',
        content: 'Secure remote control for the Pi already running on your machine.',
      },
      { name: 'twitter:image', content: 'https://pix.deepoke.com/og-image.png' },
    ],
    links: [
      { rel: 'stylesheet', href: appCss },
      { rel: 'canonical', href: 'https://pix.deepoke.com/' },
      { rel: 'icon', href: '/favicon.png', type: 'image/png' },
      { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' },
    ],
  }),
  shellComponent: RootDocument,
})

function RootDocument({ children }: { children: ReactNode }) {
  const [queryClient] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 10 * 60 * 1000,
            retry: 1,
            refetchOnWindowFocus: false,
          },
        },
      }),
  )

  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_BOOTSTRAP_SCRIPT }} />
        <HeadContent />
      </head>
      <body>
        <QueryClientProvider client={queryClient}>
          <RootProvider theme={{ enabled: false }} search={{ preload: false }}>
            <a className="skip-link" href="#main-content">Skip to main content</a>
            {children}
          </RootProvider>
        </QueryClientProvider>
        <Scripts />
      </body>
    </html>
  )
}
