import { Download as DownloadIcon } from 'lucide-react'

import { ButtonLink } from '#/components/ui/button'
import { type PixRelease } from '#/lib/release'

export function Hero({ release }: { release: PixRelease | null }) {
  return (
    <section className="hero-v2" id="top">
      <div className="hero-content-v2">
        <h1>
          Control Pi
          <br />
          <span>from anywhere.</span>
        </h1>
        <p className="hero-lede-v2">
          Pix runs next to Pi on your machine, then gives paired devices a
          direct or encrypted way to pick up the same sessions.
        </p>
        <div className="hero-actions-v2">
          <ButtonLink href="#download" variant="primary">
            <DownloadIcon size={16} strokeWidth={2} />
            Download Pix
          </ButtonLink>
          <span className="hero-version-v2">
            {release ? `v${release.version}` : 'latest version'}
          </span>
        </div>
      </div>
    </section>
  )
}
