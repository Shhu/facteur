<script setup lang="ts">
import type { Address } from '~/utils/types'

const model = defineModel<string[]>({ required: true })
const props = withDefaults(defineProps<{
  label: string
  placeholder?: string
  autofocus?: boolean
  touch?: boolean
}>(), { placeholder: undefined, autofocus: false, touch: false })
const { t } = useI18n()

const id = useId()
const input = useTemplateRef<HTMLInputElement>('input')
const term = ref('')
const focused = ref(false)
/** Escape closed the list: it stays closed until something is typed or the field is left. */
const dismissed = ref(false)
const suggestions = ref<Address[]>([])
const highlighted = ref(0)
const picked = ref<string | null>(null)

/** Latest request: an older answer arriving after it is ignored. */
let request = 0
let timer: ReturnType<typeof setTimeout> | undefined
function load(q: string, delay: number) {
  clearTimeout(timer)
  const n = ++request
  timer = setTimeout(async () => {
    try {
      // An empty query gives the recent recipients.
      const found = await api.contacts(q.trim())
      if (n === request) suggestions.value = found
    } catch {
      if (n === request) suggestions.value = []
    }
  }, delay)
}
watch(term, (q) => {
  dismissed.value = false
  load(q, q.trim() ? 150 : 0)
})

const items = computed(() => {
  const chosen = new Set(model.value.map(recipientKey))
  return suggestions.value.filter(a => !chosen.has(a.email.toLowerCase()))
})
watch(items, () => {
  highlighted.value = 0
})
const listOpen = computed(() => focused.value && !dismissed.value && items.value.length > 0)
const heading = computed(() => (term.value.trim() ? t('app.composer.contacts') : t('app.composer.recent')))
const optionId = (i: number) => `${id}-option-${i}`

function onFocus() {
  focused.value = true
  load(term.value, 0)
}
function onBlur() {
  focused.value = false
  dismissed.value = false
  picked.value = null
  if (term.value.trim()) add(term.value)
}

/** A recipient chosen in the list or confirmed with Enter: the list closes until the next key. */
function commit(text: string) {
  add(text)
  nextTick(() => {
    dismissed.value = true
  })
}

function pick(a: Address) {
  commit(addressFull(a))
  input.value?.focus()
}

/** Typed or pasted: "a@x.fr, b@y.fr; c@z.fr" gives three recipients. A quoted display name
 *  (`"Dupont, Marie" <m@x.fr>`) stays in one piece. */
function add(text: string) {
  const parts = splitRecipients(text)
  const known = new Set(model.value.map(recipientKey))
  const fresh = parts.filter((p) => {
    const key = recipientKey(p)
    if (known.has(key)) return false
    known.add(key)
    return true
  })
  if (fresh.length) model.value = [...model.value, ...fresh]
  term.value = ''
}

function remove(i: number) {
  model.value = model.value.filter((_, j) => j !== i)
  picked.value = null
}

/** Typed text that is a whole address: Enter adds it as typed rather than the first suggestion
 *  (unless that suggestion is the same address). */
const typedAddress = computed(() => {
  const email = recipientKey(term.value.trim())
  return /^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(email) ? email : null
})

function onKey(e: KeyboardEvent) {
  if (e.isComposing) return
  // Comma or semicolon commits the address being typed, as in most clients
  // (not inside a quoted name or between angle brackets).
  if ((e.key === ',' || e.key === ';') && term.value.trim() && !insideName(term.value)) {
    e.preventDefault()
    add(term.value)
    return
  }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    if (!listOpen.value) {
      dismissed.value = false
      return
    }
    e.preventDefault()
    const n = items.value.length
    highlighted.value = (highlighted.value + (e.key === 'ArrowDown' ? 1 : n - 1)) % n
    document.getElementById(optionId(highlighted.value))?.scrollIntoView({ block: 'nearest' })
    return
  }
  if (e.key === 'Enter') {
    // ⌘↵ sends: the composer's shortcut commits the field itself.
    if (e.metaKey || e.ctrlKey) return
    const choice = listOpen.value ? items.value[highlighted.value] : undefined
    if (choice && !(typedAddress.value && typedAddress.value !== choice.email.toLowerCase())) {
      e.preventDefault()
      pick(choice)
    } else if (term.value.trim()) {
      e.preventDefault()
      commit(term.value)
    }
    return
  }
  if (e.key === 'Escape' && listOpen.value) {
    e.preventDefault()
    e.stopPropagation()
    dismissed.value = true
    return
  }
  if (e.key === 'Backspace' && !term.value && model.value.length) {
    e.preventDefault()
    remove(model.value.length - 1)
  }
}

/** Splits typed or pasted text on `,` `;` and new lines, except inside a quoted display name or
 *  angle brackets. An unquoted name holding a comma (`Dupont, Marie <m@x.fr>`) is put back
 *  together: a piece without any address directly followed by `Name <address>`. */
function splitRecipients(text: string): string[] {
  const pieces: string[] = []
  let current = ''
  let quoted = false
  let angle = false
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!
    if (quoted && c === '\\') {
      current += c + (text[i + 1] ?? '')
      i++
      continue
    }
    if (c === '"') quoted = !quoted
    else if (!quoted && c === '<') angle = true
    else if (!quoted && c === '>') angle = false
    else if (!quoted && !angle && (c === ',' || c === ';' || c === '\n')) {
      pieces.push(current)
      current = ''
      continue
    }
    current += c
  }
  pieces.push(current)
  const merged: string[] = []
  for (const piece of pieces.map(p => p.trim()).filter(Boolean)) {
    const prev = merged.at(-1)
    if (prev !== undefined && !prev.includes('@') && !prev.includes('"') && /^[^<>@"]*<[^<>]+@[^<>]+>$/.test(piece)) {
      merged[merged.length - 1] = `${prev}, ${piece}`
      continue
    }
    merged.push(piece)
  }
  return merged.map(normalizeRecipient)
}

/** `Name <address>` written back in its canonical form (name quoted when it needs to be). */
function normalizeRecipient(piece: string): string {
  return /<[^<>]+>\s*$/.test(piece) ? addressFull(parseRecipient(piece)) : piece
}

function recipientKey(recipient: string): string {
  return (/<([^<>]+)>\s*$/.exec(recipient)?.[1] ?? recipient).trim().toLowerCase()
}

/** The cursor is inside a quoted name or angle brackets: `,` and `;` are part of it. */
function insideName(text: string): boolean {
  const quotes = (text.replace(/\\./g, '').match(/"/g) ?? []).length
  return quotes % 2 === 1 || text.lastIndexOf('<') > text.lastIndexOf('>')
}

/** Chips: name and address of each recipient; an address that cannot be sent to is flagged
 *  (the draft still keeps it as typed). */
const chips = computed(() => model.value.map((raw) => {
  const address = parseRecipient(raw)
  return {
    raw,
    address,
    label: props.touch ? firstName(address) : addressLabel(address),
    valid: /^[^\s@<>,;"]+@[^\s@<>,;"]+$/.test(address.email)
  }
}))

function flush() {
  clearTimeout(timer)
  if (term.value.trim()) add(term.value)
}
function focus() {
  input.value?.focus()
}
defineExpose({ flush, focus })

onMounted(() => {
  if (props.autofocus) input.value?.focus()
})
onBeforeUnmount(() => clearTimeout(timer))
</script>

<template>
  <div :class="touch ? '' : 'relative'">
    <!-- Label and trailing button stay on the first line when the chips wrap. -->
    <div
      class="flex items-start border-b transition-colors"
      :class="[focused ? 'border-primary' : 'border-default', touch ? 'gap-2.5' : 'gap-3']"
    >
      <label :for="id" class="flex h-13 shrink-0 items-center text-[13px] text-muted" :class="touch ? 'w-10' : 'w-11'">{{ label }}</label>
      <div class="flex min-h-13 min-w-0 flex-1 cursor-text flex-wrap items-center gap-1.5 py-2.75" @click.self="focus">
        <span
          v-for="(c, i) in chips"
          :key="`${i}-${c.raw}`"
          class="inline-flex h-7.5 max-w-full min-w-0 items-center gap-1.5 rounded-full border ps-0.75 text-[13px]"
          :class="[
            c.valid ? 'border-default bg-elevated text-highlighted' : 'border-error/40 bg-error/10 text-error',
            touch && picked !== c.raw ? 'pe-2.5' : 'pe-1'
          ]"
          :title="c.valid ? c.raw : $t('app.composer.invalidAddress', { address: c.raw })"
          @click="touch && (picked = picked === c.raw ? null : c.raw)"
        >
          <MailAvatar :address="c.address" class="size-6! text-[9px]!" />
          <span class="truncate">{{ c.label }}</span>
          <button
            v-if="!touch || picked === c.raw"
            type="button"
            class="flex size-5.5 shrink-0 items-center justify-center rounded-full text-dimmed hover:bg-accented/60 hover:text-default"
            :aria-label="$t('app.composer.removeRecipient', { name: c.label })"
            @mousedown.prevent
            @click.stop="remove(i)"
          >
            <UIcon name="i-lucide-x" class="size-3" />
          </button>
        </span>
        <input
          :id="id"
          ref="input"
          :value="term"
          type="text"
          role="combobox"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          enterkeyhint="next"
          :aria-expanded="listOpen"
          :aria-controls="`${id}-list`"
          :aria-activedescendant="listOpen ? optionId(highlighted) : undefined"
          :placeholder="model.length ? undefined : placeholder"
          class="h-7.5 flex-1 border-0 bg-transparent text-highlighted outline-none placeholder:text-dimmed"
          :class="[touch ? 'text-[15px]' : 'text-sm', focused || !model.length ? (touch ? 'min-w-20' : 'min-w-40') : 'min-w-4']"
          @input="term = ($event.target as HTMLInputElement).value"
          @focus="onFocus"
          @blur="onBlur"
          @keydown="onKey"
        >
      </div>
      <div v-if="$slots.trailing" class="flex h-13 shrink-0 items-center">
        <slot name="trailing" />
      </div>
    </div>

    <div
      v-if="listOpen"
      :id="`${id}-list`"
      role="listbox"
      :aria-label="heading"
      :class="touch
        ? 'flex flex-col border-b border-default py-1.5'
        : 'absolute start-14 top-[calc(100%+6px)] z-30 w-105 max-w-[calc(100%-3.5rem)] rounded-[14px] border border-default bg-default p-1.5 shadow-[0_18px_50px_rgba(24,24,27,0.18)] dark:bg-neutral-800/95 dark:shadow-[0_18px_50px_rgba(0,0,0,0.55)]'"
    >
      <span class="text-[11px] font-bold tracking-[0.06em] text-dimmed uppercase" :class="touch ? 'pt-1.5 pb-1' : 'px-2.5 pt-1.5 pb-1'">{{ heading }}</span>
      <button
        v-for="(a, i) in items"
        :id="optionId(i)"
        :key="a.email"
        type="button"
        role="option"
        :aria-selected="i === highlighted"
        tabindex="-1"
        class="flex w-full items-center gap-3 text-start"
        :class="touch
          ? 'min-h-13 py-1 active:bg-elevated/60'
          : ['min-h-12 rounded-[10px] px-2.5 py-1.5', i === highlighted ? 'bg-primary/10' : '']"
        @mousedown.prevent
        @mouseenter="highlighted = i"
        @click="pick(a)"
      >
        <MailAvatar :address="a" :class="touch ? 'size-9! text-xs!' : 'size-8! text-[11px]!'" />
        <span class="flex min-w-0 flex-1 flex-col">
          <span class="truncate font-semibold text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ a.name || a.email }}</span>
          <span v-if="a.name" class="truncate text-muted" :class="touch ? 'text-[12.5px]' : 'text-xs'">{{ a.email }}</span>
        </span>
        <UKbd v-if="!touch && i === highlighted" value="enter" />
      </button>
    </div>
  </div>
</template>
