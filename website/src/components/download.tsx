import { Check, Copy, Terminal } from 'lucide-react'
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'

import { Button, ButtonLink } from '#/components/ui/button'
import {
  GITHUB_URL,
  GITHUB_RELEASES_URL,
  findReleaseAsset,
  getLatestRelease,
  type PixRelease,
} from '#/lib/release'

const IOS_APP_URL = 'https://deepoke.com/pix'
const INSTALL_COMMAND = 'curl -fsSL https://pix.deepoke.com/install.sh | sh'
const HOMEBREW_DOCS_URL = `${GITHUB_URL}/blob/main/docs/INSTALLATION.md#homebrew`

export function Download({ release }: { release: PixRelease | null }) {
  const [copied, setCopied] = useState(false)
  const query = useQuery({
    queryKey: ['latest-release'],
    queryFn: () => getLatestRelease(),
    initialData: release,
    staleTime: 10 * 60 * 1000,
    retry: 1,
    refetchOnWindowFocus: false,
  })
  const latestRelease = query.data

  async function copyInstallCommand() {
    try {
      await navigator.clipboard.writeText(INSTALL_COMMAND)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2_000)
    } catch {
      setCopied(false)
    }
  }

  const macDmg = findReleaseAsset(latestRelease, /macos-arm64\.dmg$/i)
  const macZip = findReleaseAsset(latestRelease, /macos-arm64\.zip$/i)
  const macUrl = macDmg?.url ?? macZip?.url ?? GITHUB_RELEASES_URL
  const linuxPackagesUrl = latestRelease?.htmlUrl ?? GITHUB_RELEASES_URL

  return (
    <section className="download-v2" id="download" aria-labelledby="download-heading">
      <div className="download-content-v2">
        <div className="install-panel-v2">
          <h2 id="download-heading">Install Pix</h2>
          <div className="install-command-v2">
            <div className="install-command-toolbar-v2">
              <span className="install-command-language-v2">
                <Terminal size={14} aria-hidden="true" />
                sh
              </span>
            </div>
            <pre tabIndex={0}><code>{INSTALL_COMMAND}</code></pre>
            <Button
              className="copy-button-v2"
              variant="quiet"
              type="button"
              aria-label={copied ? 'Install command copied' : 'Copy install command'}
              title={copied ? 'Install command copied' : 'Copy install command'}
              onClick={copyInstallCommand}
            >
              {copied ? <Check size={15} /> : <Copy size={15} />}
            </Button>
          </div>
          <p className="install-note-v2">
            <span>macOS &amp; Linux</span>
            <span aria-hidden="true">·</span>
            <span>No sudo required</span>
          </p>
          <nav className="install-links-v2" aria-label="More installation options">
            <a href={HOMEBREW_DOCS_URL} target="_blank" rel="noreferrer">Homebrew</a>
            <a href={macUrl} target="_blank" rel="noreferrer">Download macOS</a>
            <a href={linuxPackagesUrl} target="_blank" rel="noreferrer">Linux packages</a>
          </nav>
        </div>
        <section className="clients-v2" aria-labelledby="clients-heading">
          <h2 id="clients-heading">Clients</h2>
          <div className="client-options-v2">
            <ButtonLink
              href={IOS_APP_URL}
              target="_blank"
              rel="noreferrer"
              variant="primary"
            >
              Download iOS
            </ButtonLink>
          </div>
        </section>
      </div>
    </section>
  )
}
