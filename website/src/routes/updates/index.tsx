import { createFileRoute } from '@tanstack/react-router'

import { Footer } from '#/components/footer'
import { Header } from '#/components/header'
import { createSeoHead, updatesStructuredData } from '#/lib/seo'
import { updateSource } from '#/lib/updates'

const TITLE = 'Pix Updates'
const DESCRIPTION = 'New features and improvements in Pix.'

function getUpdatePages() {
  return [...updateSource.getPages()].sort((left, right) => left.data.order - right.data.order)
}

type UpdatePage = ReturnType<typeof getUpdatePages>[number]

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

function UpdateCard({ page }: { page: UpdatePage }) {
  const isPreview = page.data.releaseStatus === 'preview'

  return (
    <a className="update-card" href={page.url}>
      {!isPreview ? (
        <div className="update-card-meta">
          <time dateTime={page.data.date}>{page.data.date}</time>
          <span aria-hidden="true">·</span>
          <span>{page.data.version}</span>
        </div>
      ) : null}
      <h3>{page.data.title}</h3>
      <p>{page.data.description}</p>
      {!isPreview ? (
        <div className="update-card-details">
          <span>{page.data.platform}</span>
        </div>
      ) : null}
      <span className="update-card-link">Read update <span aria-hidden="true">→</span></span>
    </a>
  )
}

function UpdateSection({
  eyebrow,
  title,
  note,
  pages,
}: {
  eyebrow: string
  title: string
  note?: string
  pages: UpdatePage[]
}) {
  return (
    <section className="updates-section" aria-label={title}>
      <div className="updates-section-heading">
        <p className="updates-eyebrow">{eyebrow}</p>
        <h2>{title}</h2>
        {note ? <p>{note}</p> : null}
      </div>
      <div className="updates-list">
        {pages.map((page) => <UpdateCard key={page.url} page={page} />)}
      </div>
    </section>
  )
}

function UpdatesIndex() {
  const pages = getUpdatePages()
  const releasedPages = pages.filter((page) => page.data.releaseStatus === 'published')
  const previewPages = pages.filter((page) => page.data.releaseStatus === 'preview')

  return (
    <div className="site-root-v2 updates-root" id="top">
      <Header />
      <main id="main-content" className="updates-page">
        <section className="updates-hero">
          <h1>Updates</h1>
          <p>{DESCRIPTION}</p>
        </section>

        {releasedPages.length > 0 ? (
          <UpdateSection eyebrow="Released" title="Latest updates" pages={releasedPages} />
        ) : null}

        {previewPages.length > 0 ? (
          <UpdateSection
            eyebrow="Coming next"
            title="Coming next"
            note="Available on main while we prepare the next Pix release."
            pages={previewPages}
          />
        ) : null}
      </main>
      <Footer />
    </div>
  )
}
