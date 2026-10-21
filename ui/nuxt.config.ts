export default defineNuxtConfig({
  compatibilityDate: '2026-10-01',
  ssr: false,
  modules: ['@nuxt/ui', '@nuxtjs/i18n'],
  css: ['~/assets/css/main.css'],
  devtools: { enabled: false },
  // Offline app: no Google fonts, icons bundled.
  ui: { fonts: false },
  // Also scan .ts files: icons named only in utils/composables were missing from the bundle.
  icon: { provider: 'none', clientBundle: { scan: { globInclude: ['**/*.{vue,ts}'] } } },
  devServer: { host: 'localhost', port: 3000 },
  vite: {
    clearScreen: false,
    envPrefix: ['VITE_', 'TAURI_'],
    server: { strictPort: true, watch: { ignored: ['**/src-tauri/**'] } }
  },
  // No DNS prefetch of links shown by the app itself (a reply's quoted mail in the editor): the
  // mail frames turn it off in their own documents.
  app: { head: { title: 'facteur', htmlAttrs: { lang: 'en' }, meta: [{ 'http-equiv': 'x-dns-prefetch-control', content: 'off' }] } },
  // Locale comes from settings, or the system language on first launch (plugins/00.language).
  i18n: {
    strategy: 'no_prefix',
    defaultLocale: 'en',
    detectBrowserLanguage: false,
    locales: ['en', 'fr'].map(code => ({
      code,
      language: code === 'fr' ? 'fr-FR' : 'en-US',
      name: code === 'fr' ? 'Français' : 'English',
      files: ['common', 'mail', 'app', 'settings', 'tools'].map(ns => `${code}/${ns}.json`)
    })),
    vueI18n: './i18n.config.ts'
  }
})
