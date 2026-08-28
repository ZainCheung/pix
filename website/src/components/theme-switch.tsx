import { Monitor, Moon, Sun } from 'lucide-react'
import { useEffect, useSyncExternalStore, type KeyboardEvent } from 'react'

import {
  THEME_OPTIONS,
  applyTheme,
  getThemePreference,
  setThemePreference,
  subscribeTheme,
  type ThemePreference,
} from '#/lib/theme'

const options: Array<{
  value: ThemePreference
  label: string
  icon: typeof Sun
}> = [
  { value: 'system', label: 'System', icon: Monitor },
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
]

export function ThemeSwitch() {
  const preference = useSyncExternalStore(subscribeTheme, getThemePreference, (): ThemePreference => 'system')

  useEffect(() => {
    applyTheme(preference)
  }, [preference])

  function move(current: ThemePreference, key: string): ThemePreference {
    const index = THEME_OPTIONS.indexOf(current)
    if (key === 'Home') return 'system'
    if (key === 'End') return 'dark'
    const delta = key === 'ArrowRight' || key === 'ArrowDown' ? 1 : -1
    return THEME_OPTIONS[(index + delta + THEME_OPTIONS.length) % THEME_OPTIONS.length] ?? current
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!['ArrowRight', 'ArrowLeft', 'ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
    event.preventDefault()
    const next = move(preference, event.key)
    setThemePreference(next)
    const buttons = event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="radio"]')
    buttons[THEME_OPTIONS.indexOf(next)]?.focus()
  }

  return (
    <div
      className="theme-switch"
      role="radiogroup"
      aria-label="Color theme"
      onKeyDown={onKeyDown}
    >
      {options.map((option) => {
        const Icon = option.icon
        const checked = preference === option.value
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={checked}
            aria-label={option.label}
            title={option.label}
            tabIndex={checked ? 0 : -1}
            onClick={() => setThemePreference(option.value)}
          >
            <Icon size={14} strokeWidth={2} />
          </button>
        )
      })}
    </div>
  )
}
