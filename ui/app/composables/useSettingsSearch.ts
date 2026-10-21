import type { ComputedRef, InjectionKey, Ref } from 'vue'

interface SectionSearch {
  /** The section's title or keywords match, so all its rows show. */
  whole: ComputedRef<boolean>
  rows: Map<symbol, ComputedRef<boolean>>
}

interface PageSearch {
  query: Ref<string>
  sections: Map<symbol, { id?: string, shown: ComputedRef<boolean> }>
}

const pageKey: InjectionKey<PageSearch> = Symbol('settings-search')
const sectionKey: InjectionKey<SectionSearch> = Symbol('settings-search-section')

export function foldText(text: string) {
  return text.normalize('NFD').replace(/\p{Diacritic}/gu, '').toLowerCase()
}

function matcher(query: Ref<string> | null, texts: () => (string | undefined)[]) {
  return computed(() => {
    const words = foldText(query?.value ?? '').split(/\s+/).filter(Boolean)
    if (!words.length) return true
    const hay = foldText(texts().filter(Boolean).join(' '))
    return words.every(w => hay.includes(w))
  })
}

export function provideSettingsSearch() {
  const query = ref('')
  const sections = shallowReactive(new Map<symbol, { id?: string, shown: ComputedRef<boolean> }>())
  provide(pageKey, { query, sections })
  const visible = computed(() => [...sections.values()].filter(s => s.shown.value))
  return {
    query,
    found: computed(() => visible.value.length > 0),
    showsSection: (id: string) => visible.value.some(s => s.id === id)
  }
}

export function useSettingsSection(texts: () => (string | undefined)[], id?: string) {
  const page = inject(pageKey, null)
  const whole = matcher(page?.query ?? null, texts)
  const rows = shallowReactive(new Map<symbol, ComputedRef<boolean>>())
  provide(sectionKey, { whole, rows })
  const shown = computed(() => whole.value || [...rows.values()].some(r => r.value))
  if (page) {
    const key = Symbol('section')
    page.sections.set(key, { id, shown })
    onBeforeUnmount(() => page.sections.delete(key))
  }
  return shown
}

export function useSettingMatch(texts: () => (string | undefined)[]) {
  const page = inject(pageKey, null)
  const section = inject(sectionKey, null)
  const own = matcher(page?.query ?? null, texts)
  if (section) {
    const id = Symbol('setting')
    section.rows.set(id, own)
    onBeforeUnmount(() => section.rows.delete(id))
  }
  return computed(() => own.value || !!section?.whole.value)
}
