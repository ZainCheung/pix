export const INSTALL_COMMAND = 'curl -fsSL https://pix.deepoke.com/install.sh | sh'
export const HOMEBREW_DOCS_URL = '/docs/installation#homebrew'
export const TESTFLIGHT_QR_SRC = '/testflight-qr.svg'
export const START_PATH = '/start'

export type SetupStep = 'computer' | 'iphone' | 'pair'
export type SetupOs = 'mac' | 'linux'

export const SETUP_STEPS: { id: SetupStep; label: string }[] = [
  { id: 'computer', label: 'Computer' },
  { id: 'iphone', label: 'iPhone' },
  { id: 'pair', label: 'Pair' },
]

export function parseSetupStep(value: unknown): SetupStep {
  return value === 'iphone' || value === 'pair' ? value : 'computer'
}

export function parseSetupOs(value: unknown): SetupOs | undefined {
  return value === 'mac' || value === 'linux' ? value : undefined
}

export function detectSetupOs(userAgent: string): SetupOs | undefined {
  const ua = userAgent.toLowerCase()
  if (/iphone|ipad|ipod/.test(ua)) return undefined
  if (/android/.test(ua)) return undefined
  if (/mac os x|macintosh/.test(ua)) return 'mac'
  if (/linux/.test(ua) || /cros/.test(ua)) return 'linux'
  return undefined
}

export function isAppleMobile(userAgent: string) {
  return /iphone|ipad|ipod/i.test(userAgent)
}

export function setupSearch(step: SetupStep, os?: SetupOs) {
  return { step, os }
}
