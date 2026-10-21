import { useLocalStorage } from '@vueuse/core'
import { effectScope, watch } from 'vue'
import type { Ref } from 'vue'
import { setWeekStart } from '~/utils/calendar'
import { setHourCycle } from '~/utils/format'
import type { HourCycle } from '~/utils/format'

/** As `Date.getDay()`, or `auto`: the language's. */
export type WeekStart = 'auto' | '1' | '6' | '0'

let prefs: { hourCycle: Ref<HourCycle>, weekStart: Ref<WeekStart> } | undefined

/** Per device; plugins/00.language.client.ts applies them at start. */
export function useDateTimePrefs() {
  prefs ??= effectScope(true).run(() => {
    const hourCycle = useLocalStorage<HourCycle>('facteur:hour-cycle', 'auto', { writeDefaults: false })
    const weekStart = useLocalStorage<WeekStart>('facteur:week-start', 'auto', { writeDefaults: false })
    watch(hourCycle, c => setHourCycle(['h12', 'h23'].includes(c) ? c : 'auto'), { immediate: true })
    watch(weekStart, d => setWeekStart(['0', '1', '6'].includes(d) ? Number(d) : null), { immediate: true })
    return { hourCycle, weekStart }
  })!
  return prefs
}
