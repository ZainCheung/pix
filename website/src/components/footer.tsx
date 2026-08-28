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
      </div>
      <div className="footer-bottom-v2">
        <span>© {new Date().getFullYear()} Pix contributors</span>
        <ThemeSwitch />
      </div>
    </footer>
  )
}
