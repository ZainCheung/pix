import { createFileRoute, notFound } from '@tanstack/react-router'
import { createServerFn } from '@tanstack/react-start'
import { Suspense, use } from 'react'

import { Footer } from '#/components/footer'
import { Header } from '#/components/header'
import { useMDXComponents } from '#/components/mdx'
import { createSeoHead, updateStructuredData } from '#/lib/seo'
import { updates, updateSource } from '#/lib/updates'

const FALLBACK_DESCRIPTION = 'Product updates and release notes from Pix.'

export const Route = createFileRoute('/updates/$slug')({
  loader: async ({ params }) => serverLoader({ data: params.slug }),
  head: ({ loaderData }) => {
    if (!loaderData) return {}

    const title = `${loaderData.title} | Pix Updates`

    return createSeoHead({
      title,
      description: loaderData.description,
      path: loaderData.url,
      type: 'article',
      structuredData: updateStructuredData({
        title,
        description: loaderData.description,
        path: loaderData.url,
        date: loaderData.date,
        updated: loaderData.updated,
        version: loaderData.version,
      }),
    })
  },
  component: UpdatePage,
})

const serverLoader = createServerFn({
  method: 'GET',
})
  .validator((slug: string) => slug)
  .handler(async ({ data: slug }) => {
    const page = updateSource.getPage([slug])
    if (!page) throw notFound()

    await updates.getPage(page.path)?.preload()

    return {
      path: page.path,
      url: page.url,
      title: page.data.title,
      description: page.data.description ?? FALLBACK_DESCRIPTION,
      date: page.data.date,
      updated: page.data.updated,
      version: page.data.version,
      platform: page.data.platform,
      status: page.data.status,
    }
  })

function Content({ path }: { path: string }) {
  const page = updates.getPage(path)
  if (!page) throw new Error(`Unknown update page: ${path}`)

  use(page.load())
  const MDX = page.body

  return (
    <div className="update-body">
      <MDX components={useMDXComponents()} />
    </div>
  )
}

function UpdatePage() {
  const data = Route.useLoaderData()

  return (
    <div className="site-root-v2 updates-root" id="top">
      <Header />
      <main id="main-content" className="update-page">
        <article className="update-article">
          <header className="update-detail-hero">
            <a className="update-back-link" href="/updates">← All updates</a>
            <p className="update-detail-meta">
              <time dateTime={data.date}>{data.date}</time>
              <span aria-hidden="true">·</span>
              <span>{data.version}</span>
            </p>
            <h1>{data.title}</h1>
            <p className="update-detail-lede">{data.description}</p>
            <div className="update-detail-details">
              <span>{data.status}</span>
              <span>{data.platform}</span>
              {data.updated && data.updated !== data.date ? (
                <span>Updated {data.updated}</span>
              ) : null}
            </div>
          </header>

          <Suspense>
            <Content path={data.path} />
          </Suspense>

          <nav className="update-links" aria-label="Related Pix pages">
            <a href="/updates">All product updates</a>
            <a href="/docs/installation">Install Pix</a>
            <a href="/use-cases">Browse use cases</a>
          </nav>
        </article>
      </main>
      <Footer />
    </div>
  )
}
