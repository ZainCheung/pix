import { createFileRoute } from '@tanstack/react-router'

import { FAQ } from '#/components/faq'
import { Capabilities, WhyPix } from '#/components/features'
import { Footer } from '#/components/footer'
import { GetStarted } from '#/components/get-started'
import { Header } from '#/components/header'
import { Hero } from '#/components/hero'
import { HowItWorks } from '#/components/how-it-works'
import { ProductPreview } from '#/components/product-preview'
import { createSeoHead, HOME_DESCRIPTION, HOME_TITLE, homeStructuredData } from '#/lib/seo'

export const Route = createFileRoute('/')({
  head: () =>
    createSeoHead({
      title: HOME_TITLE,
      description: HOME_DESCRIPTION,
      path: '/',
      structuredData: homeStructuredData(),
    }),
  component: Home,
})

function Home() {
  return (
    <div className="site-root-v2">
      <Header />
      <main id="main-content">
        <Hero />
        <ProductPreview />
        <HowItWorks />
        <Capabilities />
        <GetStarted />
        <WhyPix />
        <FAQ />
      </main>
      <Footer />
    </div>
  )
}
