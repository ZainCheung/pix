export function cn(...values: Array<string | false | null | undefined>) {
  return values.filter(Boolean).join(' ')
}

export function formatReleaseDate(value: string | null) {
  if (!value) return 'latest release'

  return new Intl.DateTimeFormat('en', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    timeZone: 'UTC',
  }).format(new Date(value))
}
