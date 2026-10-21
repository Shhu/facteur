import { invoke as tauriInvoke } from '@tauri-apps/api/core'
import type { InvokeArgs, InvokeOptions } from '@tauri-apps/api/core'

// The app calls this wrapper instead of Tauri's `invoke` so `--perf` builds can time commands
// without patching Tauri's internals (they are read-only).
type Timer = (command: string, ms: number, ok: boolean) => void
let timer: Timer | null = null

export function timeCommands(t: Timer) {
  timer = t
}

export function invoke<T>(command: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  if (!timer) return tauriInvoke<T>(command, args, options)
  const started = performance.now()
  const pending = tauriInvoke<T>(command, args, options)
  const done = (ok: boolean) => timer?.(command, performance.now() - started, ok)
  pending.then(() => done(true), () => done(false))
  return pending
}
