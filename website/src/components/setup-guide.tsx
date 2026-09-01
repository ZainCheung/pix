import { useEffect } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'

import { InstallCommand } from '#/components/install-command'
import { ButtonLink } from '#/components/ui/button'
import {
  HOMEBREW_DOCS_URL,
  SETUP_STEPS,
  TESTFLIGHT_QR_SRC,
  detectSetupOs,
  isAppleMobile,
  parseSetupOs,
  parseSetupStep,
  setupSearch,
  type SetupOs,
  type SetupStep,
} from '#/lib/install'
import {
  GITHUB_RELEASES_URL,
  findReleaseAsset,
  getLatestRelease,
  type PixRelease,
} from '#/lib/release'
import { IOS_APP_URL } from '#/lib/seo'

type SetupSearch = {
  step: SetupStep
  os?: SetupOs
}

export function SetupGuide({
  release,
  search,
}: {
  release: PixRelease | null
  search: SetupSearch
}) {
  const navigate = useNavigate({ from: '/start' })
  const step = parseSetupStep(search.step)
  const os = parseSetupOs(search.os)
  const query = useQuery({
    queryKey: ['latest-release'],
    queryFn: () => getLatestRelease(),
    initialData: release,
    staleTime: 10 * 60 * 1000,
    retry: 1,
    refetchOnWindowFocus: false,
  })
  const latestRelease = query.data
  const macDmg = findReleaseAsset(latestRelease, /macos-arm64\.dmg$/i)
  const macZip = findReleaseAsset(latestRelease, /macos-arm64\.zip$/i)
  const macUrl = macDmg?.url ?? macZip?.url ?? GITHUB_RELEASES_URL
  const linuxPackagesUrl = latestRelease?.htmlUrl ?? GITHUB_RELEASES_URL

  useEffect(() => {
    const params = new URLSearchParams(window.location.search)
    const ua = navigator.userAgent
    const nextStep = params.has('step')
      ? parseSetupStep(params.get('step'))
      : isAppleMobile(ua) ? 'iphone' : 'computer'
    const nextOs = params.has('os')
      ? parseSetupOs(params.get('os'))
      : detectSetupOs(ua)
    if (nextStep === step && nextOs === os) return
    void navigate({ search: setupSearch(nextStep, nextOs), replace: true })
  }, [navigate, os, step])

  function go(nextStep: SetupStep, nextOs = os) {
    void navigate({ search: setupSearch(nextStep, nextOs) })
  }

  return (
    <div className="setup-v2">
      <header className="setup-intro-v2">
        <div className="section-label-v2">Set up Pix</div>
        <h1>Install both sides, then pair once.</h1>
        <p>
          Pix runs on your computer and connects to Pix for iPhone.
          Pi stays on the computer.
        </p>
      </header>

      <nav className="setup-progress-v2" aria-label="Setup steps">
        {SETUP_STEPS.map((item, index) => {
          const current = item.id === step
          const done = SETUP_STEPS.findIndex((entry) => entry.id === step) > index
          return (
            <button
              key={item.id}
              type="button"
              className="setup-progress-step-v2"
              data-current={current ? 'true' : undefined}
              data-done={done ? 'true' : undefined}
              aria-current={current ? 'step' : undefined}
              onClick={() => go(item.id)}
            >
              <span>{index + 1}</span>
              {item.label}
            </button>
          )
        })}
      </nav>

      {step === 'computer' ? (
        <ComputerStep
          os={os ?? 'mac'}
          version={latestRelease?.version}
          macUrl={macUrl}
          linuxPackagesUrl={linuxPackagesUrl}
          onOsChange={(nextOs) => go('computer', nextOs)}
          onContinue={() => go('iphone')}
        />
      ) : null}

      {step === 'iphone' ? (
        <IphoneStep
          onBack={() => go('computer')}
          onContinue={() => go('pair')}
        />
      ) : null}

      {step === 'pair' ? (
        <PairStep os={os ?? 'mac'} onBack={() => go('iphone')} />
      ) : null}
    </div>
  )
}

function ComputerStep({
  os,
  version,
  macUrl,
  linuxPackagesUrl,
  onOsChange,
  onContinue,
}: {
  os: SetupOs
  version?: string
  macUrl: string
  linuxPackagesUrl: string
  onOsChange: (os: SetupOs) => void
  onContinue: () => void
}) {
  return (
    <section className="setup-panel-v2" aria-labelledby="setup-computer-heading">
      <div className="setup-os-v2" role="tablist" aria-label="Computer platform">
        <button
          type="button"
          role="tab"
          aria-selected={os === 'mac'}
          data-active={os === 'mac' ? 'true' : undefined}
          onClick={() => onOsChange('mac')}
        >
          Mac
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={os === 'linux'}
          data-active={os === 'linux' ? 'true' : undefined}
          onClick={() => onOsChange('linux')}
        >
          Linux
        </button>
      </div>

      {os === 'mac' ? (
        <>
          <h2 id="setup-computer-heading">Install Pix on this Mac</h2>
          <p>
            Pix runs alongside Pi and makes your sessions available to your
            paired iPhone.
          </p>
          <ButtonLink href={macUrl} target="_blank" rel="noreferrer" variant="primary">
            Download Pix for Mac
          </ButtonLink>
          <p className="setup-meta-v2">
            Apple Silicon · macOS Sonoma or later
            {version ? ` · v${version}` : ''}
          </p>
          <p className="setup-more-v2">
            <a href={HOMEBREW_DOCS_URL}>Homebrew and command-line installation</a>
          </p>
        </>
      ) : (
        <>
          <h2 id="setup-computer-heading">Install Pix on Linux</h2>
          <p>
            Run this on the machine where Pi already works. The installer
            sets Pix up next to Pi.
          </p>
          <InstallCommand />
          <p className="setup-meta-v2">Debian · Ubuntu · Fedora · x86_64 · ARM64</p>
          <p className="setup-more-v2">
            <a href={linuxPackagesUrl} target="_blank" rel="noreferrer">Linux packages</a>
          </p>
        </>
      )}

      <div className="setup-nav-v2">
        <button type="button" className="button button-primary" onClick={onContinue}>
          Continue to iPhone
        </button>
      </div>
    </section>
  )
}

function IphoneStep({
  onBack,
  onContinue,
}: {
  onBack: () => void
  onContinue: () => void
}) {
  return (
    <section className="setup-panel-v2" aria-labelledby="setup-iphone-heading">
      <h2 id="setup-iphone-heading">Get Pix on your iPhone</h2>
      <p>
        This is a TestFlight beta, not an App Store listing. Scan the code
        from the computer you are setting up, or open TestFlight on the phone.
      </p>
      <div className="setup-iphone-v2">
        <a className="setup-qr-v2" href={IOS_APP_URL} target="_blank" rel="noreferrer">
          <img
            src={TESTFLIGHT_QR_SRC}
            alt="QR code for the Pix TestFlight beta"
            width={196}
            height={196}
            decoding="async"
          />
          <span>Scan to open TestFlight</span>
        </a>
        <div className="setup-iphone-copy-v2">
          <ButtonLink href={IOS_APP_URL} target="_blank" rel="noreferrer" variant="primary">
            Open TestFlight
          </ButtonLink>
          <p>Public beta via TestFlight. Install TestFlight from Apple if the phone asks for it.</p>
        </div>
      </div>
      <div className="setup-nav-v2">
        <button type="button" className="button button-secondary" onClick={onBack}>
          Back
        </button>
        <button type="button" className="button button-primary" onClick={onContinue}>
          Continue to pairing
        </button>
      </div>
    </section>
  )
}

function PairStep({
  os,
  onBack,
}: {
  os: SetupOs
  onBack: () => void
}) {
  return (
    <section className="setup-panel-v2" aria-labelledby="setup-pair-heading">
      <h2 id="setup-pair-heading">Pair your iPhone</h2>
      {os === 'mac' ? (
        <ol className="setup-pair-list-v2">
          <li>
            <strong>Open Pix on your computer</strong>
            <span>Use the Pix menu bar extra after setup finishes.</span>
          </li>
          <li>
            <strong>Choose Add Device…</strong>
            <span>Keep the phone and Mac on the same Wi-Fi when you can.</span>
          </li>
          <li>
            <strong>Open Pix on your iPhone and choose this computer</strong>
            <span>If you are not on the same network, scan the QR code Pix shows on the Mac.</span>
          </li>
          <li>
            <strong>Confirm the six-digit code and approve</strong>
            <span>Approve only when the device name and code match.</span>
          </li>
        </ol>
      ) : (
        <ol className="setup-pair-list-v2">
          <li>
            <strong>Finish setup on the computer</strong>
            <span>The installer walks you through making Pix available on this machine.</span>
          </li>
          <li>
            <strong>Open Pix on your iPhone</strong>
            <span>Keep the phone and computer on the same network when you can.</span>
          </li>
          <li>
            <strong>Choose this computer, or scan the QR it prints</strong>
            <span>If you are away from that network, use the QR code from setup.</span>
          </li>
          <li>
            <strong>Confirm the six-digit code and approve</strong>
            <span>Approve the request on the computer only when the code matches.</span>
          </li>
        </ol>
      )}

      <div className="setup-ready-v2">
        <h3>You&apos;re ready.</h3>
        <p>Open Pix on your iPhone and choose this computer.</p>
      </div>

      <div className="setup-nav-v2">
        <button type="button" className="button button-secondary" onClick={onBack}>
          Back
        </button>
        <ButtonLink href="/docs/pairing" variant="quiet">
          Pairing details
        </ButtonLink>
      </div>
    </section>
  )
}
