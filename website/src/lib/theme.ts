export const THEME_STORAGE_KEY = 'pix-theme'

export type ThemePreference = 'system' | 'light' | 'dark'
export type ResolvedTheme = 'light' | 'dark'

export const THEME_OPTIONS = ['system', 'light', 'dark'] as const

const THEME_CHANGE_EVENT = 'pix-theme-change'

// Fumadocs uses the `.dark` class for its dark-mode utilities and Shiki token
// colors, while Pix uses `data-theme` for its own color tokens.
export const THEME_BOOTSTRAP_SCRIPT = `(function(){try{var t=localStorage.getItem(${JSON.stringify(THEME_STORAGE_KEY)});var p=t==="light"||t==="dark"?t:"system";var r=document.documentElement;var d=p==="dark"||(p==="system"&&window.matchMedia("(prefers-color-scheme: dark)").matches);r.setAttribute("data-theme",p);r.classList.toggle("dark",d);r.style.colorScheme=p==="system"?"light dark":p;}catch(e){var r=document.documentElement;var d=window.matchMedia("(prefers-color-scheme: dark)").matches;r.setAttribute("data-theme","system");r.classList.toggle("dark",d);r.style.colorScheme="light dark";}})();`

export function isThemePreference(value: unknown): value is ThemePreference {
  return value === 'system' || value === 'light' || value === 'dark'
}

export function getThemePreference(): ThemePreference {
  if (typeof window === 'undefined') return 'system'
  try {
    const stored = window.localStorage.getItem(THEME_STORAGE_KEY)
    return isThemePreference(stored) ? stored : 'system'
  } catch {
    return 'system'
  }
}

export function resolveTheme(preference: ThemePreference = getThemePreference()): ResolvedTheme {
  if (preference === 'light' || preference === 'dark') return preference
  if (typeof window === 'undefined') return 'light'
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function applyTheme(preference: ThemePreference) {
  if (typeof document === 'undefined') return

  const root = document.documentElement
  const resolved = resolveTheme(preference)
  root.dataset.theme = preference
  root.classList.toggle('dark', resolved === 'dark')
  root.style.colorScheme = preference === 'system' ? 'light dark' : resolved

  const themeColor = document.querySelector('meta[name="theme-color"]')
  if (themeColor) {
    themeColor.setAttribute('content', resolved === 'dark' ? '#111111' : '#ffffff')
  }
}

export function setThemePreference(preference: ThemePreference) {
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, preference)
  } catch {
    // Private mode can block storage; still apply for this session.
  }
  applyTheme(preference)
  window.dispatchEvent(new Event(THEME_CHANGE_EVENT))
}

export function subscribeTheme(onStoreChange: () => void) {
  const onChange = () => onStoreChange()
  const onStorage = (event: StorageEvent) => {
    if (event.key && event.key !== THEME_STORAGE_KEY) return
    applyTheme(getThemePreference())
    onChange()
  }

  window.addEventListener(THEME_CHANGE_EVENT, onChange)
  window.addEventListener('storage', onStorage)

  const media = window.matchMedia('(prefers-color-scheme: dark)')
  const onMedia = () => {
    if (getThemePreference() === 'system') {
      applyTheme('system')
      onChange()
    }
  }
  media.addEventListener('change', onMedia)

  return () => {
    window.removeEventListener(THEME_CHANGE_EVENT, onChange)
    window.removeEventListener('storage', onStorage)
    media.removeEventListener('change', onMedia)
  }
}
