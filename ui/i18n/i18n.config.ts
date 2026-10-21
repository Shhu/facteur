// vue-i18n options: English is the reference language; a key missing in another language falls
// back to English.
export default defineI18nConfig(() => ({
  legacy: false,
  fallbackLocale: 'en',
  missingWarn: false,
  fallbackWarn: false,
  // French counts 0 as singular (« 0 message »). With two forms, 0 and 1 take the first; with
  // three (zero | one | many), the same choice as vue-i18n's default.
  pluralRules: {
    fr: (n: number, len: number) => (len === 2 ? (Math.abs(n) > 1 ? 1 : 0) : Math.min(Math.abs(n), 2))
  }
}))
