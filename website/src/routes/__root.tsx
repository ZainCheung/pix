import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { HeadContent, Scripts, createRootRoute } from '@tanstack/react-router'
import { RootProvider } from 'fumadocs-ui/provider/tanstack'
import { useState, type ReactNode } from 'react'

import { HOME_DESCRIPTION, HOME_TITLE, siteUrl } from '#/lib/seo'
import { THEME_BOOTSTRAP_SCRIPT } from '#/lib/theme'
import appCss from '#/styles.css?url'

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1, viewport-fit=cover' },
      {
        name: 'google-site-verification',
        content: '1UCPJS3sB9WZErM1bYSAQjHPlduzWMYzhvPgogHOij8',
      },
      { title: HOME_TITLE },
      {
        name: 'description',
        content: HOME_DESCRIPTION,
      },
      { name: 'theme-color', content: '#ffffff' },
      { property: 'og:site_name', content: 'Pix' },
      { property: 'og:title', content: HOME_TITLE },
      {
        property: 'og:description',
        content: HOME_DESCRIPTION,
      },
      { property: 'og:type', content: 'website' },
      { property: 'og:image', content: siteUrl('/og-image.png') },
      { name: 'twitter:card', content: 'summary_large_image' },
      { name: 'twitter:title', content: HOME_TITLE },
      {
        name: 'twitter:description',
        content: HOME_DESCRIPTION,
      },
      { name: 'twitter:image', content: siteUrl('/og-image.png') },
    ],
    links: [
      { rel: 'stylesheet', href: appCss },
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
