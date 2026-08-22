import { createFileRoute } from '@tanstack/react-router'

import { Download } from '#/components/download'
import { FAQ } from '#/components/faq'
import { Features } from '#/components/features'
import { Footer } from '#/components/footer'
import { Header } from '#/components/header'
import { Hero } from '#/components/hero'
import { ProductPreview } from '#/components/product-preview'
import { getLatestRelease } from '#/lib/release'

export const Route = createFileRoute('/')({
  loader: () => getLatestRelease(),
  component: Home,
})

function Home() {
  const release = Route.useLoaderData()

  return (
    <div className="site-root-v2">
      <Header />
      <main id="main-content">
        <Hero release={release} />
        <ProductPreview />
        <Features />
        <Download release={release} />
        <FAQ />
      </main>
      <Footer />
    </div>
  )
}
