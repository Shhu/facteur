export default defineNuxtPlugin(() => {
  const { supported, available, checkNow, install } = useUpdater()
  if (!supported || import.meta.dev) return
  const toast = useToast()
  let offered: string | null = null
  watch(available, (update) => {
    if (!update || offered === update.version) return
    offered = update.version
    toast.add({
      id: 'facteur-update',
      title: tr('settings.about.availableTitle', { version: update.version }),
      description: tr('settings.about.availableHint'),
      icon: 'i-lucide-download',
      duration: 0,
      actions: [{ label: tr('settings.about.install'), icon: 'i-lucide-refresh-cw', color: 'primary', variant: 'solid', onClick: () => { install() } }]
    })
  })
  setTimeout(() => checkNow(), 15_000)
  setInterval(() => checkNow(), 6 * 60 * 60 * 1000)
})
