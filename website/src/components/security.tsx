import { BookOpen, ExternalLink, GitBranch, KeyRound, Lock, ShieldCheck } from 'lucide-react'

import { ButtonLink } from '#/components/ui/button'
import { GITHUB_URL } from '#/lib/release'

const securityLinks = [
  { label: 'Security policy', href: `${GITHUB_URL}/blob/main/SECURITY.md`, icon: <ShieldCheck size={15} /> },
  { label: 'Architecture', href: `${GITHUB_URL}/blob/main/docs/%28develop-pix%29/ARCHITECTURE.md`, icon: <BookOpen size={15} /> },
  { label: 'Wire protocol v1', href: `${GITHUB_URL}/blob/main/protocol/schema/v1.md`, icon: <GitBranch size={15} /> },
]

export function Security() {
  return (
    <section className="security-section" id="security" aria-labelledby="security-heading">
      <div className="section-shell security-layout">
        <div className="security-copy">
          <div className="section-kicker">05 / Security model</div>
          <h2 id="security-heading">The relay can connect you without becoming your data store.</h2>
          <p>
            Pairing and workspace approval happen at the host boundary. Once a
            device is trusted, Pix wire frames protect the connection end to
            end. A relay only needs enough metadata to join the two endpoints.
          </p>
          <div className="security-actions">
            <ButtonLink href={`${GITHUB_URL}/tree/main/docs`} target="_blank" rel="noreferrer" variant="primary">
              Read the docs <ExternalLink size={15} />
            </ButtonLink>
            <ButtonLink href={`${GITHUB_URL}/tree/main/protocol`} target="_blank" rel="noreferrer" variant="quiet">
              Inspect the protocol <GitBranch size={15} />
            </ButtonLink>
          </div>
        </div>

        <div className="security-panel">
          <div className="security-panel-heading">
            <span>trust boundary</span>
            <span className="security-live"><span className="status-dot" /> enforced</span>
          </div>
          <div className="security-steps">
            <div className="security-step">
              <span className="security-step-index">01</span>
              <span className="security-step-icon"><KeyRound size={17} /></span>
              <span><strong>Device pairing</strong><small>explicit host approval</small></span>
            </div>
            <span className="security-step-line" />
            <div className="security-step">
              <span className="security-step-index">02</span>
              <span className="security-step-icon"><Lock size={17} /></span>
              <span><strong>Encrypted channel</strong><small>same frames, local or remote</small></span>
            </div>
            <span className="security-step-line" />
            <div className="security-step">
              <span className="security-step-index">03</span>
              <span className="security-step-icon"><ShieldCheck size={17} /></span>
              <span><strong>Authorized workspace</strong><small>canonical root boundary</small></span>
            </div>
          </div>
          <div className="security-panel-note">
            <span className="security-note-mark">↳</span>
            <span>Relay receives opaque encrypted frames, not prompts, files, or model output.</span>
          </div>
        </div>
      </div>
      <div className="section-shell security-links">
        {securityLinks.map((link) => (
          <a key={link.label} href={link.href} target="_blank" rel="noreferrer">
            {link.icon}
            {link.label}
            <ExternalLink size={12} />
          </a>
        ))}
      </div>
    </section>
  )
}
