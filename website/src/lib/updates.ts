import { loader } from 'fumadocs-core/source'
import { pageSchema } from 'fumadocs-core/source/schema'
import { defineDocs } from 'fumadocs-mdx/macro'
import { z } from 'zod'

const ISO_DATE = /^\d{4}-\d{2}-\d{2}$/

const updateSchema = pageSchema.extend({
  date: z.string().regex(ISO_DATE),
  updated: z.string().regex(ISO_DATE).optional(),
  version: z.string(),
  platform: z.string(),
  status: z.string(),
  releaseStatus: z.enum(['published', 'preview']),
  order: z.number().int().positive(),
})

export type UpdatePageData = z.infer<typeof updateSchema>

export const updates = defineDocs({
  dir: 'content/updates',
  docs: {
    files: ['*.mdx'],
    async: true,
    schema: updateSchema,
    postprocess: {
      includeProcessedMarkdown: true,
    },
  },
})

export const updateSource = loader({
  baseUrl: '/updates',
  source: updates.toFumadocsSource(),
})
