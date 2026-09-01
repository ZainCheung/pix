import { createFileRoute } from '@tanstack/react-router'

import { UseCasePage, USE_CASES } from '#/components/use-case-page'
import { createSeoHead, useCaseStructuredData } from '#/lib/seo'

const page = USE_CASES['continue-pi-sessions']

export const Route = createFileRoute('/use-cases/continue-pi-sessions')({
  head: () =>
    createSeoHead({
      title: page.title,
      description: page.description,
      path: `/use-cases/${page.slug}`,
      structuredData: useCaseStructuredData({
        title: page.title,
        description: page.description,
        path: `/use-cases/${page.slug}`,
        faq: page.faq,
      }),
    }),
  component: () => <UseCasePage page={page} />,
})
