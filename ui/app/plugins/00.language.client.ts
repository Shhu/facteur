// Runs first: the other plugins already show text.
export default defineNuxtPlugin(async () => {
  await applyLanguage(preferredLanguage())
  useDateTimePrefs()
})
