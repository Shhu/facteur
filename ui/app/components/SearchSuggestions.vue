<script setup lang="ts">
import { completeOperator } from '~/utils/searchOperators'

/** Operators offered under a search field while their name is being typed (`is:`, `de`…), then
 *  their values: contacts after `de:`, `à:`, `avec:`; folders after `dans:`; dates after `après:`,
 *  `avant:`. The field forwards its keys to `onKeydown`: arrows choose, then Tab or Enter completes
 *  (Enter also right after a `:`), Escape closes. Tab completes only after an arrow: otherwise it
 *  leaves the field as usual (no keyboard trap). The field binds `combobox` as its ARIA attributes. */
const query = defineModel<string>({ required: true })
const props = defineProps<{
  focused: boolean
  accountId?: number | null
}>()
const { t } = useI18n()

const dismissed = ref<string | null>(null)
const active = ref(0)
/** Moved to with the arrows: Enter then completes instead of searching. */
const chosen = ref(false)

const { items, word, context } = useSearchSuggestions(query, () => props.focused && dismissed.value !== query.value, () => props.accountId)
// Only a new list starts over: the same one read again (a folder's count changed) keeps the choice.
watch(() => items.value.map(i => i.insert).join('\n'), () => {
  active.value = 0
  chosen.value = false
})

const listId = useId()
const optionId = (i: number) => `${listId}-${i}`
const combobox = computed(() => ({
  'role': 'combobox',
  'aria-autocomplete': 'list' as const,
  'aria-controls': listId,
  'aria-expanded': items.value.length > 0,
  'aria-activedescendant': items.value.length && chosen.value ? optionId(active.value) : undefined
}))

function pick(i: number) {
  const item = items.value[i]
  if (!item) return
  query.value = completeOperator(query.value, item.insert)
}

/** Handles a key of the field; true when it was used here. */
function onKeydown(e: KeyboardEvent): boolean {
  if (!items.value.length) return false
  // Without an arrow, Enter completes an operator's name, or a value nothing was typed for yet
  // (`de:` alone searches nothing); a value being typed is searched as it is.
  const first = items.value[active.value]
  const enterPicks = chosen.value || (word.value.includes(':') && (first?.operator || context.value?.value === ''))
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    const n = items.value.length
    active.value = (active.value + (e.key === 'ArrowDown' ? 1 : n - 1)) % n
    chosen.value = true
    nextTick(() => document.getElementById(optionId(active.value))?.scrollIntoView({ block: 'nearest' }))
  } else if ((e.key === 'Tab' && chosen.value && !e.shiftKey) || (e.key === 'Enter' && enterPicks)) {
    pick(active.value)
  } else if (e.key === 'Escape') {
    dismissed.value = query.value
  } else return false
  e.preventDefault()
  e.stopPropagation()
  return true
}
defineExpose({ onKeydown, combobox })
</script>

<template>
  <div
    v-if="items.length"
    :id="listId"
    role="listbox"
    :aria-label="t('mail.searchSuggest.label')"
    class="absolute inset-x-0 top-full z-20 mt-1.5 max-h-80 overflow-y-auto rounded-xl border border-default bg-(--surface-card) p-1 shadow-lg"
  >
    <button
      v-for="(item, i) in items"
      :key="item.insert"
      :id="optionId(i)"
      type="button"
      tabindex="-1"
      role="option"
      :aria-selected="i === active"
      class="flex w-full items-center gap-3 rounded-lg px-2.5 py-1.5 text-start text-[13px] max-lg:min-h-11"
      :class="i === active ? 'bg-primary/10' : 'hover:bg-elevated'"
      @mousedown.prevent="pick(i)"
      @mouseenter="active = i"
    >
      <span v-if="item.operator" class="shrink-0 font-mono font-semibold text-primary">{{ item.label }}</span>
      <template v-else>
        <UIcon :name="item.icon ?? 'i-lucide-search'" class="size-4 shrink-0 text-dimmed" />
        <span class="min-w-0 shrink truncate font-medium text-highlighted">{{ item.label }}</span>
      </template>
      <span class="min-w-0 flex-1 truncate text-muted">{{ item.hint }}</span>
      <UKbd v-if="i === active && chosen" value="Tab" size="sm" class="max-lg:hidden" />
    </button>
  </div>
</template>
