import { createFileRoute } from '@tanstack/react-router'

import { Footer } from '#/components/footer'
import { Header } from '#/components/header'
import { createSeoHead, updatesStructuredData } from '#/lib/seo'
import { updateSource } from '#/lib/updates'

const TITLE = 'Pix Updates: Pi Coding Agent Features and Product News'
const DESCRIPTION =
  'See what changed in Pix: Pi TUI integration, long-running session history, image attachments, and other product improvements.'

function getUpdatePages() {
  return [...updateSource.getPages()].sort((left, right) => left.data.order - right.data.order)
}

export const Route = createFileRoute('/updates/')({
  head: () =>
    createSeoHead({
      title: TITLE,
      description: DESCRIPTION,
      path: '/updates',
      structuredData: updatesStructuredData({
        title: TITLE,
        description: DESCRIPTION,
        path: '/updates',
        items: getUpdatePages().map((page) => ({
          title: page.data.title,
          path: page.url,
        })),
      }),
    }),
  component: UpdatesIndex,
})

function UpdatesIndex() {
  const pages = getUpdatePages()

  return (
    <div className="site-root-v2 updates-root" id="top">
      <Header />
      <main id="main-content" className="updates-page">
        <section className="updates-hero">
          <p className="updates-eyebrow">Product updates</p>
          <h1>What changed in Pix, and why.</h1>
          <p>
            First-party notes about new Pi capabilities, the problems they solve,
            and the design choices that keep Pix local-first. Updates explain
            product changes; the <a href="/docs">documentation</a> explains
            configuration details.
          </p>
        </section>

        <section className="updates-list" aria-label="Pix product updates">
          {pages.map((page) => (
            <a className="update-card" href={page.url} key={page.url}>
              <div className="update-card-meta">
                <time dateTime={page.data.date}>{page.data.date}</time>
                <span aria-hidden="true">·</span>
                <span>{page.data.version}</span>
              </div>
              <h2>{page.data.title}</h2>
              <p>{page.data.description}</p>
              <div className="update-card-details">
                <span>{page.data.status}</span>
                <span>{page.data.platform}</span>
              </div>
              <span className="update-card-link">Read update <span aria-hidden="true">→</span></span>
            </a>
          ))}
        </section>

        <p className="updates-note">
          Looking for installation or command details? Start with the{' '}
          <a href="/docs/installation">installation guide</a> or browse the{' '}
          <a href="/docs/pi-tui-bridge">Pi TUI bridge documentation</a>.
        </p>
      </main>
      <Footer />
    </div>
  )
}
