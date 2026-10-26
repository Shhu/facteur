import { invoke } from '~/utils/ipc'

// Off unless turned on in Settings > About. Never in the way of the app: any failure is only logged.
export default defineNuxtPlugin(async () => {
  const on = await invoke<boolean>('perf_enabled').catch(() => false)
  if (on) await usePerf().setEnabled(true, false)
})
