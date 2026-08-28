import { ArrowDownToLine, ArrowUpRight, Check, ChevronDown, Copy } from 'lucide-react'
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'

import { Button, ButtonLink } from '#/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '#/components/ui/tabs'
import {
  GITHUB_RELEASES_URL,
  findReleaseAsset,
  getLatestRelease,
  type PixRelease,
} from '#/lib/release'

const installCommands = {
  macos: 'brew tap ZainCheung/pix https://github.com/ZainCheung/pix.git\nbrew install --cask ZainCheung/pix/pix',
  linux: 'curl -fsSL https://pix.deepoke.com/install.sh | sh',
} as const

const IOS_APP_URL = 'https://deepoke.com/pix'

type InstallPlatform = 'macos' | 'linux' | 'ios'

function assetOrFallback(release: PixRelease | null, pattern: RegExp) {
  return findReleaseAsset(release, pattern)?.url ?? GITHUB_RELEASES_URL
}

function PlatformLink({ label, detail, href }: { label: string; detail: string; href: string }) {
  return (
    <a className="download-platform-v2" href={href} target="_blank" rel="noreferrer">
      <span><strong>{label}</strong><small>{detail}</small></span>
    </a>
  )
}

export function Download({ release }: { release: PixRelease | null }) {
  const [copied, setCopied] = useState(false)
  const [installPlatform, setInstallPlatform] = useState<InstallPlatform>('macos')
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
    if (installPlatform === 'ios') return
    const command = installCommands[installPlatform]

    try {
      await navigator.clipboard.writeText(command)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2_000)
    } catch {
      setCopied(false)
    }
  }

  const macDmg = findReleaseAsset(latestRelease, /macos-arm64\.dmg$/i)
  const macZip = findReleaseAsset(latestRelease, /macos-arm64\.zip$/i)
  const macUrl = macDmg?.url ?? macZip?.url ?? GITHUB_RELEASES_URL
  const macDetail = macDmg
    ? 'DMG · drag to Applications'
    : macZip
      ? 'ZIP · Pix.app + CLI'
      : 'GitHub Releases · arm64'
  const linuxX64Url = assetOrFallback(latestRelease, /x86_64-unknown-linux-gnu\.tar\.gz$/i)
  const linuxArmUrl = assetOrFallback(latestRelease, /aarch64-unknown-linux-gnu\.tar\.gz$/i)
  const debUrl = assetOrFallback(latestRelease, /_(amd64|arm64)\.deb$/i)
  const rpmUrl = assetOrFallback(latestRelease, /\.(x86_64|aarch64)\.rpm$/i)

  return (
    <section className="download-v2" id="download" aria-labelledby="download-heading">
      <div className="download-content-v2">
        <div className="download-header-v2">
          <div>
            <div className="section-label-v2">Installation</div>
            <h2 id="download-heading">Install Pix</h2>
          </div>
          <details className="download-menu-v2">
            <summary className="download-button-v2 button button-primary">
              <ArrowDownToLine size={16} />
              Download
              <ChevronDown size={14} />
            </summary>
            <div className="download-menu-panel-v2">
              <PlatformLink label="macOS Apple Silicon" detail={macDetail} href={macUrl} />
              <PlatformLink label="Linux x86_64" detail="tar.gz · GNU userspace" href={linuxX64Url} />
              <PlatformLink label="Linux ARM64" detail="tar.gz · GNU userspace" href={linuxArmUrl} />
              <details className="download-more-v2">
                <summary>Package formats</summary>
                <PlatformLink label="Debian / Ubuntu" detail=".deb · amd64 or arm64" href={debUrl} />
                <PlatformLink label="Fedora / RHEL" detail=".rpm · x86_64 or aarch64" href={rpmUrl} />
              </details>
              <a className="download-all-v2" href={latestRelease?.htmlUrl ?? GITHUB_RELEASES_URL} target="_blank" rel="noreferrer">View all releases</a>
            </div>
          </details>
        </div>
        <Tabs
          className="install-tabs-v2"
          variant="line"
          value={installPlatform}
          onValueChange={(value) => {
            setInstallPlatform(value as InstallPlatform)
            setCopied(false)
          }}
        >
          <TabsList className="install-tabs-list-v2" aria-label="Install Pix">
            <TabsTrigger className="install-tab-trigger-v2" value="macos">macOS</TabsTrigger>
            <TabsTrigger className="install-tab-trigger-v2" value="linux">Linux</TabsTrigger>
            <TabsTrigger className="install-tab-trigger-v2" value="ios">iOS</TabsTrigger>
          </TabsList>
          <TabsContent className="install-tab-content-v2" value="macos">
            <div className="install-command-v2">
              <pre tabIndex={0}><code>{installCommands.macos}</code></pre>
              <Button
                className="copy-button-v2"
                variant="quiet"
                type="button"
                aria-label={copied ? 'Homebrew command copied' : 'Copy Homebrew command'}
                onClick={copyInstallCommand}
              >
                {copied ? <Check size={15} /> : <Copy size={15} />}
                <span>{copied ? 'Copied' : 'Copy'}</span>
              </Button>
            </div>
          </TabsContent>
          <TabsContent className="install-tab-content-v2" value="linux">
            <div className="install-command-v2">
              <pre tabIndex={0}><code>{installCommands.linux}</code></pre>
              <Button
                className="copy-button-v2"
                variant="quiet"
                type="button"
                aria-label={copied ? 'Linux command copied' : 'Copy Linux command'}
                onClick={copyInstallCommand}
              >
                {copied ? <Check size={15} /> : <Copy size={15} />}
                <span>{copied ? 'Copied' : 'Copy'}</span>
              </Button>
            </div>
          </TabsContent>
          <TabsContent className="install-tab-content-v2" value="ios">
            <div className="install-app-link-v2">
              <div>
                <strong>Pix for iPhone</strong>
                <span>Open the Pix app, then pair it with the host on your computer.</span>
              </div>
              <ButtonLink href={IOS_APP_URL} variant="primary">
                Open Pix
                <ArrowUpRight size={16} />
              </ButtonLink>
            </div>
          </TabsContent>
        </Tabs>
      </div>
    </section>
  )
}
