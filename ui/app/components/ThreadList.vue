<script setup lang="ts">
import { useVirtualizer } from '@tanstack/vue-virtual'
import type { ListItem } from '~/utils/types'

export type ListAction = 'archive' | 'trash' | 'read' | 'flag' | 'snooze' | 'unsnooze'

const props = defineProps<{
  items: ListItem[]
  opened: number | null
  selected: Set<string>
  /** Unified inbox: archiving is called "Done". */
  doneMode?: boolean
  /** Done view: the action moves back to the inbox. */
  restoreMode?: boolean
  /** Pinned threads, sorted first by the engine, get their own header. */
  pinnedFirst?: boolean
  label?: string
}>()
const emit = defineEmits<{
  /** Arrow keys on the list: the next (1) or previous (-1) thread opens, like `j` and `k`. */
  step: [dir: 1 | -1]
  open: [item: ListItem]
  toggle: [item: ListItem, range: boolean]
  action: [kind: ListAction, item: ListItem]
  more: []
}>()
const { t } = useI18n()

type Row =
  | { kind: 'header', key: string, label: string }
  | { kind: 'item', key: string, label: string, item: ListItem }

/** Date headers (pinned ones apart, first) and rows, flat. The first header is not a row: the
 *  sticky band at the top shows it. */
const rows = computed<Row[]>(() => {
  const out: Row[] = []
  let current: string | null = null
  for (const item of props.items) {
    const label = props.pinnedFirst && item.flagged ? t('mail.threadList.pinned') : dateGroup(item.activity)
    if (label !== current) {
      // Keyed by the row it heads: a label can come back further down (pinned threads first).
      if (current !== null) out.push({ kind: 'header', key: `h:${item.key}`, label })
      current = label
    }
    out.push({ kind: 'item', key: item.key, label, item })
  }
  return out
})

/** Estimated heights (measured later); the sticky band's serves as scroll margin. */
const HEADER_H = 34
const ROW_H = 88
const scroller = useTemplateRef<HTMLElement>('scroller')
const virtualizer = useVirtualizer(computed(() => ({
  count: rows.value.length,
  getScrollElement: () => scroller.value,
  estimateSize: (i: number) => (rows.value[i]?.kind === 'header' ? HEADER_H : ROW_H),
  getItemKey: (i: number) => rows.value[i]?.key ?? i,
  overscan: 8,
  scrollMargin: HEADER_H
})))
const visible = computed(() => virtualizer.value.getVirtualItems())
/** Scroll position, kept from the scroll event (the virtualizer only notifies on range changes). */
const scrollTop = ref(0)
/** Header of the first visible row, shown in the sticky band. The virtual items include the
 *  overscan rows above the view, so the row is the first one ending below the band; a date
 *  header there still belongs to the group above until it reaches the band. */
const currentLabel = computed(() => {
  const line = scrollTop.value + HEADER_H
  const v = visible.value.find(v => v.end > line) ?? visible.value[0]
  if (!v) return rows.value[0]?.label ?? ''
  const row = rows.value[v.index]!
  return (row.kind === 'header' && v.start > line - 1 ? rows.value[v.index - 1]?.label : row.label) ?? ''
})

function measure(el: unknown) {
  if (el) virtualizer.value.measureElement(el as Element)
}

const selecting = computed(() => props.selected.size > 0)
/** Hovered row: only it carries the action buttons (hundreds of rows otherwise). */
const hovered = ref<string | null>(null)
watch(() => props.opened, (id) => {
  if (id === null) return
  const index = rows.value.findIndex(r => r.kind === 'item' && r.item.threadId === id)
  if (index !== -1) virtualizer.value.scrollToIndex(index, { align: 'auto' })
})

const idPrefix = useId()
const rowId = (item: ListItem) => `${idPrefix}-${item.key}`
/** The open thread's row, when rendered (the virtualizer only keeps those near the view). */
const activeId = computed(() => {
  if (props.opened === null) return undefined
  for (const v of visible.value) {
    const row = rows.value[v.index]
    if (row?.kind === 'item' && row.item.threadId === props.opened) return rowId(row.item)
  }
  return undefined
})
function onKeydown(e: KeyboardEvent) {
  // Keys on a row's own buttons are theirs.
  if (e.target !== e.currentTarget || e.altKey || e.ctrlKey || e.metaKey) return
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault()
    emit('step', e.key === 'ArrowDown' ? 1 : -1)
  } else if (e.key === ' ') {
    const item = props.items.find(i => i.threadId === props.opened)
    if (!item) return
    e.preventDefault()
    emit('toggle', item, e.shiftKey)
  }
}

function onClick(e: MouseEvent, item: ListItem) {
  // A tap leaves no hover behind (its actions would sit under the next tap).
  if ((e as PointerEvent).pointerType !== 'mouse') hovered.value = null
  if (e.metaKey || e.ctrlKey) emit('toggle', item, false)
  else if (e.shiftKey) emit('toggle', item, true)
  else emit('open', item)
}

/** Drag data type, read by the sidebar (layouts/default.vue). */
const DRAG_TYPE = 'application/x-facteur-messages'

function onDragStart(e: DragEvent, item: ListItem) {
  if (!e.dataTransfer) return
  // A checked row carries the whole selection.
  const dragged = props.selected.has(item.key) ? props.items.filter(i => props.selected.has(i.key)) : [item]
  const accounts = new Set(dragged.map(i => i.accountId))
  e.dataTransfer.effectAllowed = 'move'
  const boxes = new Set(dragged.map(i => i.mailboxId))
  e.dataTransfer.setData(DRAG_TYPE, JSON.stringify({
    accountId: accounts.size === 1 ? dragged[0]!.accountId : null,
    ids: dragged.flatMap(i => i.ids),
    // The folder they are dragged out of (Gmail: the label they leave).
    box: boxes.size === 1 ? dragged[0]!.mailboxId : null
  }))
  // A label following the pointer, rather than the image of the whole row.
  const ghost = document.createElement('div')
  ghost.textContent = dragged.length > 1 ? t('mail.threadList.conversations', { n: dragged.length }) : (item.subject || t('common.noSubject'))
  ghost.className = 'fixed -top-96 max-w-64 truncate rounded-md bg-primary px-2.5 py-1 text-sm font-medium text-inverted shadow'
  document.body.appendChild(ghost)
  e.dataTransfer.setDragImage(ghost, 12, 12)
  setTimeout(() => ghost.remove(), 0)
}

function onScroll(e: Event) {
  const el = e.target as HTMLElement
  scrollTop.value = el.scrollTop
  if (el.scrollTop + el.clientHeight > el.scrollHeight - 400) emit('more')
}

const actions = (item: ListItem) => [
  props.restoreMode
    ? { kind: 'archive' as const, icon: 'i-lucide-inbox', label: t('mail.threadList.toInbox') }
    : props.doneMode
      ? { kind: 'archive' as const, icon: 'i-lucide-check', label: t('mail.threadList.done') }
      : { kind: 'archive' as const, icon: 'i-lucide-archive', label: t('mail.threadList.archive') },
  item.snoozedUntil
    ? { kind: 'unsnooze' as const, icon: 'i-lucide-alarm-clock-off', label: t('mail.threadList.unsnooze') }
    : { kind: 'snooze' as const, icon: 'i-lucide-alarm-clock', label: t('mail.threadList.later') },
  { kind: 'trash' as const, icon: 'i-lucide-trash-2', label: t('mail.threadList.trash') },
  {
    kind: 'read' as const,
    icon: item.unread ? 'i-lucide-mail-open' : 'i-lucide-mail',
    label: item.unread ? t('mail.threadList.markRead') : t('mail.threadList.markUnread')
  },
  { kind: 'flag' as const, icon: item.flagged ? 'i-lucide-pin-off' : 'i-lucide-pin', label: item.flagged ? t('mail.threadList.unpin') : t('mail.threadList.pin') }
]
</script>

<template>
  <!-- Background: `--list-bg`, set by the mail page (list column tone, page tone on mobile). -->
  <div ref="scroller" class="h-full overflow-y-auto" @scroll.passive="onScroll">
    <h3
      v-if="rows.length"
      class="sticky top-0 z-10 flex items-end bg-(--list-bg)/90 px-[18px] pb-1.5 text-[11px] font-bold tracking-[0.06em] text-muted uppercase backdrop-blur"
      :style="{ height: `${HEADER_H}px` }"
    >
      <span class="truncate">{{ currentLabel }}</span>
    </h3>
    <div
      role="listbox"
      tabindex="0"
      :aria-label="label"
      aria-multiselectable="true"
      :aria-activedescendant="activeId"
      class="relative w-full rounded-xl outline-offset-[-2px]"
      :style="{ height: `${virtualizer.getTotalSize()}px` }"
      @keydown="onKeydown"
    >
      <!-- Spacing lives in padding, not margins: the virtualizer measures the wrapper's box. -->
      <div
        v-for="v in visible"
        :key="String(v.key)"
        :ref="measure"
        :data-index="v.index"
        role="presentation"
        class="absolute left-0 top-0 w-full px-2"
        :class="rows[v.index]!.kind === 'item' && 'pb-0.5'"
        :style="{ transform: `translateY(${v.start - HEADER_H}px)` }"
      >
        <!-- Visual grouping only: a listbox holds options, and each row says its date. -->
        <h3
          v-if="rows[v.index]!.kind === 'header'"
          aria-hidden="true"
          class="flex items-end px-2.5 pb-1.5 text-[11px] font-bold tracking-[0.06em] text-muted uppercase"
          :style="{ height: `${HEADER_H}px` }"
        >
          <span class="truncate">{{ rows[v.index]!.label }}</span>
        </h3>
        <template v-else-if="rows[v.index]!.kind === 'item'">
          <ThreadRow
            :item="(rows[v.index] as Extract<Row, { kind: 'item' }>).item"
            :opened="opened"
            :selected="selected"
            :selecting="selecting"
            :hovered="hovered === (rows[v.index] as Extract<Row, { kind: 'item' }>).item.key"
            :actions="actions"
            :row-id="rowId((rows[v.index] as Extract<Row, { kind: 'item' }>).item)"
            @click="onClick"
            @dragstart="onDragStart"
            @hover="(key) => hovered = key"
            @toggle="(item, range) => emit('toggle', item, range)"
            @action="(kind, item) => emit('action', kind, item)"
          />
        </template>
      </div>
    </div>
    <!-- End of list: older history, server search… provided by the page. -->
    <slot name="footer" />
    <div class="h-20 lg:hidden" />
  </div>
</template>
