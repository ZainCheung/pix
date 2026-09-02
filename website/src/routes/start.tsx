import { createFileRoute } from '@tanstack/react-router'

import { Footer } from '#/components/footer'
import { Header } from '#/components/header'
import { SetupGuide } from '#/components/setup-guide'
import { parseSetupOs, parseSetupStep } from '#/lib/install'
import { getLatestRelease } from '#/lib/release'
import { createSeoHead } from '#/lib/seo'

const TITLE = 'Set up Pix'
const DESCRIPTION =
  'Install Pix on your computer and iPhone, pair them once, and start using Pi from your phone.'

export const Route = createFileRoute('/start')({
  validateSearch: (search: Record<string, unknown>) => ({
    step: parseSetupStep(search.step),
    os: parseSetupOs(search.os),
  }),
  head: () =>
    createSeoHead({
      title: TITLE,
      description: DESCRIPTION,
      path: '/start',
    }),
  loader: () => getLatestRelease(),
  component: StartPage,
})

function StartPage() {
  const release = Route.useLoaderData()
  const search = Route.useSearch()

  return (
    <div className="site-root-v2">
      <Header />
      <main id="main-content">
        <SetupGuide release={release} search={search} />
      </main>
      <Footer />
    </div>
  )
}
