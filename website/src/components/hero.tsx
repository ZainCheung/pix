import { ButtonLink } from '#/components/ui/button'
import { START_PATH } from '#/lib/install'
import { HOME_DESCRIPTION } from '#/lib/seo'

export function Hero() {
  return (
    <section className="hero-v2" id="top">
      <div className="hero-content-v2">
        <h1>
          Control Pi{' '}
          <br />
          <span>from anywhere.</span>
        </h1>
        <p className="hero-lede-v2">{HOME_DESCRIPTION}</p>
        <div className="hero-actions-v2">
          <ButtonLink href={START_PATH} variant="primary">
            Get Started
          </ButtonLink>
          <ButtonLink href="#demo" variant="secondary">
            Watch demo
          </ButtonLink>
        </div>
        <p className="hero-note-v2">Set up Pix on your computer and iPhone.</p>
      </div>
    </section>
  )
}
