import { useLocalStorage } from '@vueuse/core'
import { computed, effectScope } from 'vue'
import type { WritableComputedRef } from 'vue'

const switches = new Map<string, WritableComputedRef<boolean>>()

/** On by default; one shared state per key, created outside the caller's scope so it keeps
 *  saving after the caller unmounts. */
export function useDeviceSwitch(key: string): WritableComputedRef<boolean> {
  let enabled = switches.get(key)
  if (!enabled) {
    enabled = effectScope(true).run(() => {
      const raw = useLocalStorage(key, 'on', { writeDefaults: false })
      return computed({
        get: () => raw.value !== 'off',
        set: (value: boolean) => {
          raw.value = value ? 'on' : 'off'
        }
      })
    })!
    switches.set(key, enabled)
  }
  return enabled
}
