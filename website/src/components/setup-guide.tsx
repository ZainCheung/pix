import { useEffect, useState } from 'react'
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
  const [onPhone, setOnPhone] = useState(false)
  const query = useQuery({
    queryKey: ['latest-release'],
    queryFn: () => getLatestRelease(),
    initialData: release ?? undefined,
    staleTime: 10 * 60 * 1000,
    retry: 1,
    refetchOnWindowFocus: false,
  })
  const latestRelease = query.data ?? null
  const macDmg = findReleaseAsset(latestRelease, /macos-arm64\.dmg$/i)
  const macZip = findReleaseAsset(latestRelease, /macos-arm64\.zip$/i)
  const macUrl = macDmg?.url ?? macZip?.url ?? GITHUB_RELEASES_URL
  const linuxPackagesUrl = latestRelease?.htmlUrl ?? GITHUB_RELEASES_URL

  useEffect(() => {
    const params = new URLSearchParams(window.location.search)
    const ua = navigator.userAgent
    setOnPhone(isAppleMobile(ua))
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
          const previous = SETUP_STEPS.findIndex((entry) => entry.id === step) > index
          return (
            <button
              key={item.id}
              type="button"
              className="setup-progress-step-v2"
              data-current={current ? 'true' : undefined}
              data-previous={previous ? 'true' : undefined}
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
          os={os}
          version={latestRelease?.version}
          macUrl={macUrl}
          linuxPackagesUrl={linuxPackagesUrl}
          onOsChange={(nextOs) => go('computer', nextOs)}
          onContinue={() => go('iphone')}
        />
      ) : null}

      {step === 'iphone' ? (
        <IphoneStep
          hideQr={onPhone}
          onBack={() => go('computer')}
          onContinue={() => go('pair')}
        />
      ) : null}

      {step === 'pair' ? (
        <PairStep
          os={os}
          onOsChange={(nextOs) => go('pair', nextOs)}
          onBack={() => go('iphone')}
        />
      ) : null}
    </div>
  )
}

function OsToggle({
  os,
  onChange,
  label,
}: {
  os?: SetupOs
  onChange: (os: SetupOs) => void
  label: string
}) {
  return (
    <div className="setup-os-v2" role="tablist" aria-label={label}>
      <button
        type="button"
        role="tab"
        aria-selected={os === 'mac'}
        data-active={os === 'mac' ? 'true' : undefined}
        onClick={() => onChange('mac')}
      >
        Mac
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={os === 'linux'}
        data-active={os === 'linux' ? 'true' : undefined}
        onClick={() => onChange('linux')}
      >
        Linux
      </button>
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
  os?: SetupOs
  version?: string
  macUrl: string
  linuxPackagesUrl: string
  onOsChange: (os: SetupOs) => void
  onContinue: () => void
}) {
  return (
    <section className="setup-panel-v2" aria-labelledby="setup-computer-heading">
      {os == null ? (
        <>
          <h2 id="setup-computer-heading">Choose your computer</h2>
          <p>Pix currently supports Apple silicon Macs and Linux.</p>
          <div className="setup-os-pick-v2">
            <button type="button" className="button button-secondary" onClick={() => onOsChange('mac')}>
              Mac
            </button>
            <button type="button" className="button button-secondary" onClick={() => onOsChange('linux')}>
              Linux
            </button>
          </div>
        </>
      ) : (
        <>
          <OsToggle os={os} onChange={onOsChange} label="Computer platform" />
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
              <p className="setup-constraint-v2">
                Apple silicon · macOS Sonoma or later
                {version ? ` · v${version}` : ''}
              </p>
              <p className="setup-more-v2">
                <a href={HOMEBREW_DOCS_URL}>Homebrew and command-line installation</a>
                <span aria-hidden="true"> · </span>
                <a href="/docs/platform-support">Intel Mac? Build from source</a>
              </p>
            </>
          ) : (
            <>
              <h2 id="setup-computer-heading">Install Pix on Linux</h2>
              <p>
                Run this on the machine where Pi already works. The installer
                sets up Pix on the same machine as Pi.
              </p>
              <InstallCommand />
              <p className="setup-meta-v2">Debian · Ubuntu · Fedora · x86_64 · ARM64</p>
              <p className="setup-more-v2">
                <a href={linuxPackagesUrl} target="_blank" rel="noreferrer">Linux packages</a>
              </p>
            </>
          )}
        </>
      )}

      <div className="setup-nav-v2">
        <button type="button" className="button button-primary" onClick={onContinue}>
          I&apos;ve installed Pix — Continue
        </button>
      </div>
    </section>
  )
}

function IphoneStep({
  hideQr,
  onBack,
  onContinue,
}: {
  hideQr: boolean
  onBack: () => void
  onContinue: () => void
}) {
  return (
    <section className="setup-panel-v2" aria-labelledby="setup-iphone-heading">
      <h2 id="setup-iphone-heading">Get Pix on your iPhone</h2>
      <p>
        {hideQr
          ? 'This is a TestFlight beta, not an App Store listing. Open TestFlight on this iPhone to install Pix.'
          : 'This is a TestFlight beta, not an App Store listing. Scan the code from the computer you are setting up, or open TestFlight on the phone.'}
      </p>
      <div className={hideQr ? 'setup-iphone-v2 setup-iphone-v2-phone' : 'setup-iphone-v2'}>
        {hideQr ? null : (
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
        )}
        <div className="setup-iphone-copy-v2">
          <ButtonLink href={IOS_APP_URL} target="_blank" rel="noreferrer" variant="primary">
            Open TestFlight
          </ButtonLink>
          <p>Public beta via TestFlight. Install TestFlight from Apple if the phone asks for it.</p>
          {hideQr ? (
            <p className="setup-iphone-hint-v2">
              Setting up from a computer? Open{' '}
              <a href="https://pix.deepoke.com/start">pix.deepoke.com/start</a>
              {' '}there.
            </p>
          ) : null}
        </div>
      </div>
      <div className="setup-nav-v2">
        <button type="button" className="button button-secondary" onClick={onBack}>
          Back
        </button>
        <button type="button" className="button button-primary" onClick={onContinue}>
          I&apos;ve installed Pix — Continue to pairing
        </button>
      </div>
    </section>
  )
}

function PairStep({
  os,
  onOsChange,
  onBack,
}: {
  os?: SetupOs
  onOsChange: (os: SetupOs) => void
  onBack: () => void
}) {
  return (
    <section className="setup-panel-v2" aria-labelledby="setup-pair-heading">
      <h2 id="setup-pair-heading">Pair your iPhone</h2>
      <ol className="setup-pair-list-v2">
        <li>
          <strong>Open Pix on your computer</strong>
          <span>Keep the phone and computer nearby when you can.</span>
        </li>
        <li>
          <strong>Start pairing / Add Device</strong>
          <span>Start the pairing flow on the computer, then wait for the iPhone.</span>
        </li>
        <li>
          <strong>Open Pix on your iPhone</strong>
          <span>Use the same Pix app you just installed from TestFlight.</span>
        </li>
        <li>
          <strong>Choose the computer, or scan its QR code</strong>
          <span>Same network can discover the computer. Away from it, scan the QR Pix shows.</span>
        </li>
        <li>
          <strong>Confirm the six-digit code</strong>
          <span>Approve on the computer only when the device name and code match.</span>
        </li>
      </ol>

      <div className="setup-pair-os-v2">
        <p>Using:</p>
        <OsToggle os={os} onChange={onOsChange} label="Pairing instructions for" />
        {os === 'mac' ? (
          <p className="setup-pair-os-note-v2">Use the Pix menu bar app.</p>
        ) : null}
        {os === 'linux' ? (
          <p className="setup-pair-os-note-v2">Follow the terminal setup output.</p>
        ) : null}
      </div>

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
