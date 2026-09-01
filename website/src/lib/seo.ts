export const SITE_URL = 'https://pix.deepoke.com'
export const SITE_NAME = 'Pix'
export const GITHUB_URL = 'https://github.com/ZainCheung/pix'
export const IOS_APP_URL = 'https://deepoke.com/pix'

export const HOME_TITLE = 'Pix — Use Pi from Your iPhone'
export const HOME_DESCRIPTION =
  'Pix connects your iPhone to the Pi coding agent running on your Mac or Linux machine, so you can start, resume, and control local sessions remotely.'

export type JsonLdValue =
  | string
  | number
  | boolean
  | null
  | JsonLdObject
  | JsonLdValue[]

export type JsonLdObject = {
  [key: string]: JsonLdValue | undefined
}

export type SeoInput = {
  title: string
  description: string
  path: string
  type?: 'website' | 'article'
  image?: string
  structuredData?: JsonLdObject
}

export type SeoFaqItem = {
  question: string
  answer: string
}

export function siteUrl(path = '/') {
  if (path === '/') return `${SITE_URL}/`
  return `${SITE_URL}/${path.replace(/^\/+|\/+$/g, '')}`
}

export function createSeoHead({
  title,
  description,
  path,
  type = 'website',
  image = siteUrl('/og-image.png'),
  structuredData,
}: SeoInput) {
  const canonical = siteUrl(path)
  const meta = [
    { title },
    { name: 'description', content: description },
    { property: 'og:site_name', content: SITE_NAME },
    { property: 'og:title', content: title },
    { property: 'og:description', content: description },
    { property: 'og:type', content: type },
    { property: 'og:url', content: canonical },
    { property: 'og:image', content: image },
    { name: 'twitter:card', content: 'summary_large_image' },
    { name: 'twitter:title', content: title },
    { name: 'twitter:description', content: description },
    { name: 'twitter:image', content: image },
  ]

  return {
    meta,
    links: [{ rel: 'canonical', href: canonical }],
    ...(structuredData
      ? {
          scripts: [
            {
              type: 'application/ld+json',
              children: JSON.stringify(structuredData),
            },
          ],
        }
      : {}),
  }
}

export function homeStructuredData(): JsonLdObject {
  return {
    '@context': 'https://schema.org',
    '@graph': [
      {
        '@type': 'SoftwareApplication',
        name: SITE_NAME,
        applicationCategory: 'DeveloperApplication',
        operatingSystem: 'iOS, macOS, Linux',
        description: HOME_DESCRIPTION,
        url: siteUrl('/'),
        downloadUrl: IOS_APP_URL,
        sameAs: [GITHUB_URL],
      },
      {
        '@type': 'WebSite',
        '@id': `${siteUrl('/')}#website`,
        name: SITE_NAME,
        url: siteUrl('/'),
      },
    ],
  }
}

export function docsStructuredData({
  title,
  description,
  path,
}: Pick<SeoInput, 'title' | 'description' | 'path'>): JsonLdObject {
  const url = siteUrl(path)

  return {
    '@context': 'https://schema.org',
    '@graph': [
      {
        '@type': 'TechArticle',
        '@id': `${url}#article`,
        headline: title,
        description,
        url,
        isPartOf: { '@id': `${siteUrl('/')}#website` },
        publisher: {
          '@type': 'Organization',
          name: SITE_NAME,
          url: siteUrl('/'),
        },
        inLanguage: 'en',
      },
      {
        '@type': 'BreadcrumbList',
        itemListElement: [
          { '@type': 'ListItem', position: 1, name: SITE_NAME, item: siteUrl('/') },
          { '@type': 'ListItem', position: 2, name: 'Documentation', item: siteUrl('/docs') },
          { '@type': 'ListItem', position: 3, name: title, item: url },
        ],
      },
    ],
  }
}

export function useCaseStructuredData({
  title,
  description,
  path,
  faq = [],
}: Pick<SeoInput, 'title' | 'description' | 'path'> & {
  faq?: SeoFaqItem[]
}): JsonLdObject {
  const url = siteUrl(path)
  const breadcrumbItems = path === '/use-cases'
    ? [
        { '@type': 'ListItem', position: 1, name: SITE_NAME, item: siteUrl('/') },
        { '@type': 'ListItem', position: 2, name: 'Use cases', item: url },
      ]
    : [
        { '@type': 'ListItem', position: 1, name: SITE_NAME, item: siteUrl('/') },
        { '@type': 'ListItem', position: 2, name: 'Use cases', item: siteUrl('/use-cases') },
        { '@type': 'ListItem', position: 3, name: title, item: url },
      ]
  const graph: JsonLdValue[] = [
    {
      '@type': 'WebPage',
      '@id': `${url}#webpage`,
      name: title,
      description,
      url,
      isPartOf: { '@id': `${siteUrl('/')}#website` },
      inLanguage: 'en',
    },
    {
      '@type': 'BreadcrumbList',
      itemListElement: breadcrumbItems,
    },
  ]

  if (faq.length > 0) {
    graph.push({
      '@type': 'FAQPage',
      mainEntity: faq.map((item) => ({
        '@type': 'Question',
        name: item.question,
        acceptedAnswer: {
          '@type': 'Answer',
          text: item.answer,
        },
      })),
    })
  }

  return {
    '@context': 'https://schema.org',
    '@graph': graph,
  }
}
