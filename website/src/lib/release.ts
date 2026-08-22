import { createServerFn } from '@tanstack/react-start'

export const GITHUB_REPOSITORY = 'ZainCheung/pix'
export const GITHUB_URL = `https://github.com/${GITHUB_REPOSITORY}`
export const GITHUB_RELEASES_URL = `${GITHUB_URL}/releases/latest`

export type PixReleaseAsset = {
  name: string
  url: string
  size: number
}

export type PixRelease = {
  version: string
  htmlUrl: string
  publishedAt: string | null
  assets: PixReleaseAsset[]
}

type GitHubReleasePayload = {
  tag_name?: unknown
  html_url?: unknown
  published_at?: unknown
  assets?: unknown
}

type GitHubAssetPayload = {
  name?: unknown
  browser_download_url?: unknown
  size?: unknown
}

type CloudflareRequestInit = RequestInit & {
  cf?: {
    cacheEverything?: boolean
    cacheTtl?: number
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function parseAsset(value: unknown): PixReleaseAsset | null {
  if (!isRecord(value)) return null

  const asset = value as GitHubAssetPayload
  if (typeof asset.name !== 'string') return null
  if (typeof asset.browser_download_url !== 'string') return null

  return {
    name: asset.name,
    url: asset.browser_download_url,
    size: typeof asset.size === 'number' ? asset.size : 0,
  }
}

function parseRelease(payload: unknown): PixRelease | null {
  if (!isRecord(payload)) return null

  const release = payload as GitHubReleasePayload
  if (typeof release.tag_name !== 'string') return null

  const assets = Array.isArray(release.assets)
    ? release.assets.flatMap((asset) => {
        const parsed = parseAsset(asset)
        return parsed ? [parsed] : []
      })
    : []

  return {
    version: release.tag_name.replace(/^v/, ''),
    htmlUrl:
      typeof release.html_url === 'string'
        ? release.html_url
        : GITHUB_RELEASES_URL,
    publishedAt:
      typeof release.published_at === 'string' ? release.published_at : null,
    assets,
  }
}

export function findReleaseAsset(
  release: PixRelease | null,
  pattern: RegExp,
) {
  return release?.assets.find((asset) => pattern.test(asset.name))
}

export const getLatestRelease = createServerFn({ method: 'GET' }).handler(
  async (): Promise<PixRelease | null> => {
    const token = process.env.GITHUB_TOKEN
    const headers: Record<string, string> = {
      Accept: 'application/vnd.github+json',
      'User-Agent': 'Pix-Website',
      'X-GitHub-Api-Version': '2022-11-28',
    }
    if (token) headers.Authorization = `Bearer ${token}`

    try {
      const response = await fetch(
        `https://api.github.com/repos/${GITHUB_REPOSITORY}/releases/latest`,
        {
          headers,
          signal: AbortSignal.timeout(5_000),
          cf: { cacheEverything: true, cacheTtl: 600 },
        } satisfies CloudflareRequestInit,
      )

      if (!response.ok) return null
      return parseRelease(await response.json())
    } catch {
      // Release data is an enhancement. The page remains useful with the
      // stable GitHub Releases fallback when the API is unavailable.
      return null
    }
  },
)
