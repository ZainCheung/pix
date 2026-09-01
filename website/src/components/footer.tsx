import { BrandMark } from '#/components/ui/brand-mark'
import { ThemeSwitch } from '#/components/theme-switch'

export function Footer() {
  return (
    <footer className="site-footer-v2">
      <div className="footer-main-v2">
        <a className="brand-link-v2" href="#top" aria-label="Pix home">
          <BrandMark />
          <span>Pix</span>
        </a>
        <nav className="footer-nav-v2" aria-label="Footer navigation">
          <a href="/use-cases">Use cases</a>
          <a href="/updates">Updates</a>
          <a href="/docs">Docs</a>
        </nav>
      </div>
      <div className="footer-bottom-v2">
        <span>© {new Date().getFullYear()} Pix contributors</span>
        <ThemeSwitch />
      </div>
    </footer>
  )
}
