import twColors from 'tailwindcss/colors'

export const accentColors = [
  'red', 'orange', 'amber', 'yellow', 'lime', 'green', 'emerald', 'teal',
  'cyan', 'sky', 'blue', 'indigo', 'violet', 'purple', 'fuchsia', 'pink', 'rose'
] as const
export const neutralColors = ['zinc', 'slate', 'gray', 'neutral', 'stone'] as const

export type Accent = typeof accentColors[number]
export type Neutral = typeof neutralColors[number]

const KEY = 'facteur:appearance'

export function swatch(color: string): string {
  return (twColors as unknown as Record<string, Record<number, string>>)[color]?.[500] ?? '#888'
}

/** Applied live through Nuxt UI's app config. */
export function useAppearance() {
  const appConfig = useAppConfig()

  const accent = computed<Accent>({
    get: () => appConfig.ui.colors.primary as Accent,
    set: (value) => {
      appConfig.ui.colors.primary = value
      save()
    }
  })
  const neutral = computed<Neutral>({
    get: () => appConfig.ui.colors.neutral as Neutral,
    set: (value) => {
      appConfig.ui.colors.neutral = value
      save()
    }
  })

  function save() {
    try {
      localStorage.setItem(KEY, JSON.stringify({ accent: appConfig.ui.colors.primary, neutral: appConfig.ui.colors.neutral }))
    } catch {
      // Storage unavailable: the choice lasts for the session.
    }
  }

  function restore() {
    try {
      const saved = JSON.parse(localStorage.getItem(KEY) ?? '{}') as { accent?: string, neutral?: string }
      if (accentColors.includes(saved.accent as Accent)) appConfig.ui.colors.primary = saved.accent!
      if (neutralColors.includes(saved.neutral as Neutral)) appConfig.ui.colors.neutral = saved.neutral!
    } catch {
      // Unreadable value: keep the default colours.
    }
  }

  return { accent, neutral, restore }
}
