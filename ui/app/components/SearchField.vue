<script setup lang="ts">
import { createReusableTemplate, useResizeObserver } from '@vueuse/core'
import type { QueryPiece } from '~/utils/searchOperators'
import { folderRole, joinQuery, queryPieces, splitQuery } from '~/utils/searchOperators'

/** Finished operators show as blocks: `[Pinned] [From: Marie] invoice`. The query stays the text
 *  typed (the v-model, sent as is to the engine): the blocks only show it. The input holds what is
 *  being typed, at the end of the query or in place of a block put back into editing (Backspace
 *  in the empty input, a click on the block). */
const query = defineModel<string>({ required: true })
const props = withDefaults(defineProps<{
  placeholder: string
  variant?: 'soft' | 'outline'
  /** Folders offered after `dans:`: this account's; every account's when null. */
  accountId?: number | null
}>(), { variant: 'outline', accountId: null })
const emit = defineEmits<{ submit: [], escape: [] }>()
const { t } = useI18n()
const names = useState<Record<string, string>>('search-contact-names', () => ({}))

const input = useTemplateRef<HTMLInputElement>('input')
const scroller = useTemplateRef<HTMLElement>('scroller')
const suggestions = useTemplateRef('suggestions')
const focused = ref(false)
/** The whole query read out by screen readers: the blocks are not part of the input's value. */
const descId = useId()

/** The query in three parts: blocks, the input, blocks after the one being edited. Each is a
 *  piece of the query as typed, which is always `joinQuery(head, draft, tail)`. */
const head = ref('')
const draft = ref('')
const tail = ref('')
const composed = () => joinQuery(head.value, draft.value, tail.value)

/** Field left (or query changed from outside): blocks up to the last operator, the rest typed. */
function resplit() {
  const parts = splitQuery(query.value, !focused.value)
  head.value = parts.head
  draft.value = parts.draft
  tail.value = ''
}
watch(query, (q) => {
  if (q !== composed()) resplit()
  nextTick(measure)
}, { immediate: true })

/** Typed in the input, or completed from the list: what got finished joins the blocks. */
const typed = computed({
  get: () => draft.value,
  set: (text: string) => {
    const parts = splitQuery(text)
    head.value = joinQuery(head.value, parts.head)
    draft.value = parts.draft
    query.value = composed()
    nextTick(reveal)
  }
})

/** Read on every input event, not through v-model, which waits for the end of a composition
 *  (Android keyboards keep the word being typed composing): while composing, only the draft
 *  follows the text, so the IME text is not rewritten; the split into blocks runs at its end. */
function onInput(e: Event) {
  const el = e.target as HTMLInputElement
  if ((e as InputEvent).isComposing) {
    draft.value = el.value
    query.value = composed()
  } else typed.value = el.value
}

const before = computed(() => queryPieces(head.value))
const after = computed(() => queryPieces(tail.value, query.value.length - tail.value.length))

function edit(piece: QueryPiece) {
  const q = query.value
  head.value = q.slice(0, piece.start)
  draft.value = piece.raw
  tail.value = q.slice(piece.end)
  query.value = composed()
  focus()
}

function remove(piece: QueryPiece) {
  const q = query.value
  if (piece.end <= head.value.length) head.value = head.value.slice(0, piece.start) + head.value.slice(piece.end)
  else {
    const offset = q.length - tail.value.length
    tail.value = tail.value.slice(0, piece.start - offset) + tail.value.slice(piece.end - offset)
  }
  query.value = composed()
  focus()
}

function focus() {
  nextTick(() => {
    const el = input.value
    if (!el) return
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
    reveal()
  })
}

function reveal() {
  const box = scroller.value
  const el = input.value
  if (!box || !el) return
  if (el.offsetLeft < box.scrollLeft) box.scrollLeft = el.offsetLeft
  else if (el.offsetLeft + el.offsetWidth > box.scrollLeft + box.clientWidth) box.scrollLeft = el.offsetLeft + el.offsetWidth - box.clientWidth
  measure()
}

const cut = ref({ start: false, end: false })
function measure() {
  const box = scroller.value
  if (!box) return
  cut.value = { start: box.scrollLeft > 1, end: box.scrollLeft + box.clientWidth < box.scrollWidth - 1 }
}
useResizeObserver(scroller, measure)
const fade = computed(() => {
  const { start, end } = cut.value
  if (!start && !end) return undefined
  const mask = `linear-gradient(to right, transparent, #000 ${start ? 16 : 0}px, #000 calc(100% - ${end ? 16 : 0}px), transparent)`
  return { maskImage: mask, WebkitMaskImage: mask }
})
/** The input as wide as its text (or room for the caret); left empty, it gives its place up. */
const inputWidth = computed(() => (draft.value ? `${draft.value.length + 1}ch` : focused.value ? '3ch' : undefined))

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || suggestions.value?.onKeydown(e)) return
  const last = before.value.at(-1)
  if (e.key === 'Backspace' && !draft.value && last) {
    e.preventDefault()
    edit(last)
  } else if (e.key === 'Enter') emit('submit')
  else if (e.key === 'Escape') emit('escape')
}

function onFocus() {
  focused.value = true
  reveal()
}
function onBlur() {
  focused.value = false
  resplit()
  nextTick(measure)
}

/** A click beside the blocks types in the field, as in an input. */
function onFieldDown(e: MouseEvent) {
  if ((e.target as HTMLElement).closest('input, button')) return
  e.preventDefault()
  focus()
}

/** A block in the interface language: `From:` and the contact's name rather than the address. */
function label(piece: QueryPiece): { op: string, value?: string } {
  const chip = piece.chip
  if (chip.kind === 'flag') return { op: t(`app.palette.filter.${chip.flag}`) }
  if (chip.kind !== 'operator') return { op: chip.value }
  const op = t(`app.palette.filter.${chip.operator}`)
  const v = chip.value
  if (chip.operator === 'from' || chip.operator === 'to' || chip.operator === 'with') return { op, value: names.value[v.toLowerCase()] || v }
  if (chip.operator === 'in') {
    const role = folderRole(v)
    return { op, value: role ? roleLabels[role] : v }
  }
  if ((chip.operator === 'after' || chip.operator === 'before') && /^\d{4}-\d{2}-\d{2}$/.test(v)) {
    const d = new Date(`${v}T00:00`)
    if (!Number.isNaN(d.getTime())) return { op, value: dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(d) }
  }
  return { op, value: v }
}

// Addresses of the blocks not named yet (a search from the address bar, a saved one): their
// contact's name, looked up once.
watch(() => [...before.value, ...after.value].flatMap((p) => {
  const c = p.chip
  return c.kind === 'operator' && ['from', 'to', 'with'].includes(c.operator) && c.value.includes('@') ? [c.value.toLowerCase()] : []
}), (emails) => {
  for (const email of emails) {
    if (email in names.value) continue
    names.value[email] = ''
    api.contacts(email).then((found) => {
      const name = found.find(a => a.email.toLowerCase() === email)?.name
      if (name) names.value[email] = name
    }).catch(() => {})
  }
}, { immediate: true })

const field = computed(() => props.variant === 'soft'
  ? 'h-[38px] rounded-[10px] bg-elevated text-[13px] max-lg:h-11 max-lg:bg-(--surface-card) max-lg:ring-1 max-lg:ring-inset max-lg:ring-default max-lg:text-base'
  : 'h-8 rounded-md bg-default text-base/5 ring ring-inset ring-accented focus-within:ring-primary')

const [DefinePiece, Piece] = createReusableTemplate<{ piece: QueryPiece }>()

defineExpose({ focus })
</script>

<template>
  <div class="relative">
    <span :id="descId" class="sr-only">{{ query }}</span>
    <DefinePiece v-slot="{ piece }">
      <button
        v-if="piece.chip.kind === 'word'"
        type="button"
        tabindex="-1"
        class="shrink-0 whitespace-nowrap"
        :aria-label="t('mail.searchField.edit', { chip: piece.raw })"
        @mousedown.prevent
        @click="edit(piece)"
      >{{ piece.raw }}</button>
      <span
        v-else
        class="inline-flex h-6 max-w-64 shrink-0 max-lg:max-w-[60vw] items-center rounded-md bg-primary-100 text-[12px] text-primary-700 max-lg:h-7 max-lg:text-sm dark:bg-primary/14 dark:text-primary-300"
        :title="piece.raw"
      >
        <button
          type="button"
          tabindex="-1"
          class="flex h-full min-w-0 items-center gap-1 ps-2 pe-0.5"
          :aria-label="t('mail.searchField.edit', { chip: piece.raw })"
          @mousedown.prevent
          @click="edit(piece)"
        >
          <b class="shrink-0 font-semibold">{{ label(piece).op }}</b>
          <span v-if="label(piece).value" class="truncate">{{ label(piece).value }}</span>
        </button>
        <button
          type="button"
          tabindex="-1"
          class="flex h-full shrink-0 items-center ps-0.5 pe-1.5 opacity-60 hover:opacity-100"
          :aria-label="t('mail.searchField.remove', { chip: piece.raw })"
          @mousedown.prevent
          @click="remove(piece)"
        >
          <UIcon name="i-lucide-x" class="size-3" />
        </button>
      </span>
    </DefinePiece>
    <div
      class="flex w-full cursor-text items-center gap-1.5 px-2.5 text-highlighted outline-primary/25 transition-colors focus-within:outline-3"
      :class="field"
      @mousedown="onFieldDown"
    >
      <UIcon name="i-lucide-search" class="size-5 shrink-0 text-dimmed" />
      <div
        ref="scroller"
        class="relative flex h-full min-w-0 flex-1 items-center gap-1 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
        :style="fade"
        @scroll.passive="measure"
      >
        <Piece v-for="piece in before" :key="piece.start" :piece="piece" />
        <input
          ref="input"
          :value="draft"
          type="text"
          enterkeyhint="search"
          autocomplete="off"
          spellcheck="false"
          :placeholder="query ? undefined : placeholder"
          :aria-label="placeholder"
          :aria-describedby="query && query !== draft ? descId : undefined"
          v-bind="suggestions?.combobox"
          class="h-full min-w-0 bg-transparent outline-none placeholder:text-dimmed"
          :class="tail ? 'flex-none' : 'flex-1'"
          :style="tail ? { width: inputWidth ?? '3ch' } : { minWidth: inputWidth }"
          @input="onInput"
          @compositionend="typed = ($event.target as HTMLInputElement).value"
          @keydown="onKeydown"
          @focus="onFocus"
          @blur="onBlur"
        >
        <Piece v-for="piece in after" :key="piece.start" :piece="piece" />
      </div>
      <div class="flex shrink-0 items-center gap-1">
        <slot name="trailing" :empty="!query" />
        <SearchHelp :query="query" />
      </div>
    </div>
    <SearchSuggestions ref="suggestions" v-model="typed" :focused="focused" :account-id="accountId" />
  </div>
</template>
