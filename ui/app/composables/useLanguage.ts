const KEY = 'facteur:language'
export type Language = 'fr' | 'en'

export function preferredLanguage(): Language {
  try {
    const saved = localStorage.getItem(KEY)
    if (saved === 'fr' || saved === 'en') return saved
  } catch {
    // Storage unavailable: system language.
  }
  return navigator.language.toLowerCase().startsWith('fr') ? 'fr' : 'en'
}

/** UI, date and number formats, and the engine's texts. */
export async function applyLanguage(code: Language) {
  const { $i18n } = useNuxtApp()
  await $i18n.setLocale(code)
  setFormatLanguage(code, (key, params, plural) =>
    plural === undefined ? $i18n.t(key, params ?? {}) : $i18n.t(key, params ?? {}, plural))
  document.documentElement.lang = code
  await api.setSetting('language', code).catch(() => {})
}

export function useLanguage() {
  const { $i18n } = useNuxtApp()
  const language = computed(() => $i18n.locale.value as Language)

  async function set(code: Language) {
    try {
      localStorage.setItem(KEY, code)
    } catch {
      // Storage unavailable: the choice lasts for the session.
    }
    await applyLanguage(code)
  }

  return { language, set }
}
