import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared'

import { BrandMark } from '#/components/ui/brand-mark'
import { GITHUB_URL } from '#/lib/release'

export function baseOptions(): BaseLayoutProps {
  return {
    githubUrl: GITHUB_URL,
    nav: {
      title: (
        <span className="docs-brand">
          <BrandMark />
          <span>Pix</span>
        </span>
      ),
      url: '/',
    },
    themeSwitch: {
      enabled: false,
    },
  }
}
