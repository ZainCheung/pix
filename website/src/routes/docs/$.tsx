import { createFileRoute, notFound } from '@tanstack/react-router'
import { createServerFn } from '@tanstack/react-start'
import { useFumadocsLoader } from 'fumadocs-core/source/client'
import { DocsLayout } from 'fumadocs-ui/layouts/docs'
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
} from 'fumadocs-ui/layouts/docs/page'
import { Suspense, use } from 'react'

import { useMDXComponents } from '#/components/mdx'
import { baseOptions } from '#/lib/layout.shared'
import { createSeoHead, docsStructuredData } from '#/lib/seo'
import { docs, source } from '#/lib/source'

export const Route = createFileRoute('/docs/$')({
  loader: async ({ params }) => {
    const slugs = params._splat?.split('/').filter(Boolean) ?? []
    const data = await serverLoader({ data: slugs })
    await docs.getPage(data.path)?.preload()
    return data
  },
  head: ({ loaderData }) => {
    if (!loaderData) return {}

    const title = loaderData.title === 'Pix documentation'
      ? loaderData.title
      : `${loaderData.title} | Pix`

    return createSeoHead({
      title,
      description: loaderData.description,
      path: loaderData.url,
      type: 'article',
      structuredData: docsStructuredData({
        title,
        description: loaderData.description,
        path: loaderData.url,
      }),
    })
  },
  component: Page,
})

const serverLoader = createServerFn({
  method: 'GET',
})
  .validator((slugs: string[]) => slugs)
  .handler(async ({ data: slugs }) => {
    const page = source.getPage(slugs)
    if (!page) throw notFound()

    return {
      path: page.path,
      url: page.url,
      title: page.data.title ?? 'Pix documentation',
      description:
        page.data.description ??
        'Documentation for installing, using, and developing Pix with Pi.',
      pageTree: await source.serializePageTree(source.getPageTree()),
    }
  })

function Content({ path }: { path: string }) {
  const page = docs.getPage(path)
  if (!page) throw new Error(`Unknown docs page: ${path}`)

  const { toc } = use(page.load())
  const MDX = page.body

  return (
    <DocsPage toc={toc}>
      <span id="main-content" tabIndex={-1} className="sr-only" />
      <DocsTitle>{page.title}</DocsTitle>
      <DocsDescription>{page.description}</DocsDescription>
      <DocsBody>
        <MDX components={useMDXComponents()} />
      </DocsBody>
    </DocsPage>
  )
}

function Page() {
  const data = useFumadocsLoader(Route.useLoaderData())

  return (
    <DocsLayout {...baseOptions()} tree={data.pageTree}>
      <Suspense>
        <Content path={data.path} />
      </Suspense>
    </DocsLayout>
  )
}
