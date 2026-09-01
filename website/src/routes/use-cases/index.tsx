import { createFileRoute } from '@tanstack/react-router'

import { Footer } from '#/components/footer'
import { Header } from '#/components/header'
import { USE_CASES } from '#/components/use-case-page'
import { createSeoHead, useCaseStructuredData } from '#/lib/seo'

const TITLE = 'Pi Coding Agent Use Cases for iPhone and Remote Access | Pix'
const DESCRIPTION =
  'Answer four Pi coding agent questions: use Pi on iPhone, connect remotely, resume the same session, and understand Pix’s local-first data boundary.'

export const Route = createFileRoute('/use-cases/')({
  head: () =>
    createSeoHead({
      title: TITLE,
      description: DESCRIPTION,
      path: '/use-cases',
      structuredData: useCaseStructuredData({
        title: TITLE,
        description: DESCRIPTION,
        path: '/use-cases',
      }),
    }),
  component: UseCasesIndex,
})

function UseCasesIndex() {
  const pages = Object.values(USE_CASES)

  return (
    <div className="site-root-v2 use-case-root" id="top">
      <Header />
      <main id="main-content" className="use-cases-index-page">
        <section className="use-cases-index-hero">
          <p className="use-case-eyebrow">Pi + remote access</p>
          <h1>Pi Coding Agent use cases for iPhone and remote access</h1>
          <p>
            Pix connects the Pi coding agent on your Mac or Linux machine to a
            paired iPhone. Each guide answers a different question about using
            Pi remotely: the iPhone workflow, network access, session handoff,
            and the local-first security boundary.
          </p>
        </section>

        <section className="use-cases-grid" aria-label="Pix use cases">
          {pages.map((page) => (
            <a className="use-case-card" href={`/use-cases/${page.slug}`} key={page.slug}>
              <span className="use-case-card-index">{page.eyebrow}</span>
              <h2>{page.h1}</h2>
              <p>{page.description}</p>
              <span className="use-case-card-link">Read use case <span aria-hidden="true">→</span></span>
            </a>
          ))}
        </section>

        <p className="use-cases-index-note">
          Start with the <a href="/docs/installation">installation guide</a>,
          then choose a connection path in the <a href="/docs/remote-access">remote access docs</a>.
        </p>
      </main>
      <Footer />
    </div>
  )
}
