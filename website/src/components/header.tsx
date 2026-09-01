import { BrandMark } from '#/components/ui/brand-mark'
import { GithubMark } from '#/components/ui/github-mark'
import { GITHUB_URL } from '#/lib/release'

export function Header() {
  return (
    <header className="site-header-v2">
      <div className="header-inner-v2">
        <a className="brand-link-v2" href="#top" aria-label="Pix home">
          <BrandMark />
          <span>Pix</span>
        </a>

        <nav className="header-actions-v2" aria-label="Primary navigation">
          <a className="header-docs-v2" href="/docs">
            Docs
          </a>
          <a
            className="header-github-v2"
            href={GITHUB_URL}
            target="_blank"
            rel="noreferrer"
            aria-label="View Pix on GitHub"
          >
            <GithubMark />
          </a>
        </nav>
      </div>
    </header>
  )
}
