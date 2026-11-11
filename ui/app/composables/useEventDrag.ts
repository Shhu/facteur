import { useEventListener } from '@vueuse/core'
import type { MaybeRefOrGetter } from 'vue'
import type { CalendarEvent } from '~/utils/types'

export type DragMode = 'move' | 'start' | 'end'
/** Where the pointer is: a day of the view, and the minutes into it when the view has hours. */
export interface DragSpot { day: number, minutes: number | null }

const SNAP = 15
const THRESHOLD = 4
const EDGE = 40

interface Drag {
  event: CalendarEvent
  /** The view's days when the drag started: another range cancels it. */
  days: Date[]
  mode: DragMode
  /** Moved by days only: shown in a band or a month cell. */
  daysOnly: boolean
  x: number
  y: number
  from: DragSpot
  started: boolean
}

function snapped(d: Date): Date {
  const r = new Date(d)
  r.setMinutes(Math.round(r.getMinutes() / SNAP) * SNAP, 0, 0)
  return r
}

/** Moving and resizing events with the mouse. `locate` reads the view's spot under a pointer;
 *  `commit` writes the new times (it throws when that failed). */
export function useEventDrag(opts: {
  days: MaybeRefOrGetter<Date[]>
  events: MaybeRefOrGetter<CalendarEvent[]>
  locate: (x: number, y: number) => DragSpot | null
  commit: (e: CalendarEvent, start: number, end: number) => Promise<void>
  scroller?: MaybeRefOrGetter<HTMLElement | null | undefined>
  /** What covers the top of the scroller (its sticky header), in pixels. */
  covered?: () => number
}) {
  let drag: Drag | null = null
  /** A drag cancelled with the button still down: the click of its release is not one. */
  let cancelled = false
  const preview = ref<{ key: string, start: number, end: number } | null>(null)
  /** Written, and shown so until the events are read again: one per event. */
  const held = ref(new Map<string, { start: number, end: number }>())
  function release(key: string, p: { start: number, end: number }) {
    if (held.value.get(key) !== p) return
    const next = new Map(held.value)
    next.delete(key)
    held.value = next
  }
  const dragging = ref(false)
  let suppress = false
  let pointer = { x: 0, y: 0 }
  let frame = 0

  function times(d: Drag, to: DragSpot): { start: number, end: number } {
    const e = d.event
    const start = new Date(e.start * 1000)
    const end = new Date(e.end * 1000)
    const dayShift = to.day - d.from.day
    if (d.daysOnly || to.minutes === null || d.from.minutes === null) {
      return { start: unix(addDays(start, dayShift)), end: unix(addDays(end, dayShift)) }
    }
    const day = d.days[to.day]
    if (d.mode === 'move') {
      // The move is snapped, not the time: back where it was, an event at 10:05 stays there.
      const shift = Math.round((to.minutes - d.from.minutes) / SNAP) * SNAP
      if (!shift && !dayShift) return { start: e.start, end: e.end }
      const moved = addDays(start, dayShift)
      moved.setMinutes(moved.getMinutes() + shift)
      const s = shift ? snapped(moved) : moved
      return { start: unix(s), end: unix(s) + (e.end - e.start) }
    }
    if (!day) return { start: e.start, end: e.end }
    const at = unix(snapped(new Date(day.getFullYear(), day.getMonth(), day.getDate(), 0, to.minutes)))
    // The edge held, not moved past its own quarter hour: left as it is.
    const edge = d.mode === 'end' ? e.end : e.start
    if (at === unix(snapped(new Date(edge * 1000)))) return { start: e.start, end: e.end }
    if (d.mode === 'end') return { start: e.start, end: Math.max(at, e.start + SNAP * 60) }
    return { start: Math.min(at, e.end - SNAP * 60), end: e.end }
  }

  function follow() {
    const d = drag
    if (!d?.started) return
    const spot = opts.locate(pointer.x, pointer.y)
    if (!spot) return
    preview.value = { key: eventKey(d.event), ...times(d, spot) }
  }

  /** Scrolls the hours while the pointer stays near the top or bottom edge. */
  function edgeScroll() {
    frame = 0
    const el = toValue(opts.scroller)
    if (!drag?.started || drag.daysOnly || !el) return
    const r = el.getBoundingClientRect()
    const top = r.top + (opts.covered?.() ?? 0) + EDGE
    const step = pointer.y < top ? -Math.ceil((top - pointer.y) / 4) : pointer.y > r.bottom - EDGE ? Math.ceil((pointer.y - r.bottom + EDGE) / 4) : 0
    if (!step) return
    el.scrollTop += step
    follow()
    frame = requestAnimationFrame(edgeScroll)
  }

  function start(ev: PointerEvent, event: CalendarEvent, mode: DragMode, daysOnly: boolean) {
    if (ev.button !== 0 || ev.pointerType === 'touch' || !event.editable) return
    const from = opts.locate(ev.clientX, ev.clientY)
    if (!from) return
    cancelled = false
    drag = { event, days: [...toValue(opts.days)], mode, daysOnly, x: ev.clientX, y: ev.clientY, from, started: false }
    pointer = { x: ev.clientX, y: ev.clientY }
    if (mode !== 'move') ev.stopPropagation()
  }

  function stop() {
    drag = null
    dragging.value = false
    preview.value = null
    if (frame) cancelAnimationFrame(frame)
    frame = 0
    document.documentElement.style.removeProperty('cursor')
    document.documentElement.classList.remove('select-none')
  }

  useEventListener(window, 'pointermove', (ev: PointerEvent) => {
    const d = drag
    if (!d) return
    // The button let go where no pointerup reached us.
    if ((ev.buttons & 1) === 0) {
      cancel()
      return
    }
    pointer = { x: ev.clientX, y: ev.clientY }
    if (!d.started) {
      if (Math.hypot(ev.clientX - d.x, ev.clientY - d.y) < THRESHOLD) return
      d.started = true
      dragging.value = true
      document.documentElement.style.cursor = d.mode === 'move' ? 'grabbing' : 'ns-resize'
      document.documentElement.classList.add('select-none')
    }
    follow()
    if (!frame) frame = requestAnimationFrame(edgeScroll)
  })

  /** The click that ends a drag must not also open the event or start a new one. */
  function blockClick() {
    suppress = true
    setTimeout(() => (suppress = false), 0)
  }

  function cancel() {
    if (drag?.started) cancelled = true
    stop()
  }

  useEventListener(window, 'pointerup', () => {
    const d = drag
    const p = preview.value
    stop()
    if (cancelled) {
      cancelled = false
      blockClick()
      return
    }
    if (!d?.started) return
    blockClick()
    if (!p || (p.start === d.event.start && p.end === d.event.end)) return
    const placed = { start: p.start, end: p.end }
    held.value = new Map(held.value).set(p.key, placed)
    opts.commit(d.event, p.start, p.end).then(
      // Shown at its new place until the events are read again with it; not forever.
      () => setTimeout(() => release(p.key, placed), 10_000),
      () => release(p.key, placed)
    )
  })

  useEventListener(window, 'keydown', (ev: KeyboardEvent) => {
    if (ev.key !== 'Escape' || !drag) return
    ev.preventDefault()
    ev.stopPropagation()
    cancel()
  }, { capture: true })

  useEventListener(window, 'blur', stop)
  // The system took the gesture (a pan, a native drag): no pointerup will come.
  useEventListener(window, 'pointercancel', cancel)
  useEventListener(window, 'dragstart', cancel)
  watch(() => toValue(opts.days), cancel)
  // Released once the events read again carry the new times.
  watch(() => toValue(opts.events), (events) => {
    for (const [key, h] of held.value) {
      if (events.some(e => eventKey(e) === key && e.start === h.start && e.end === h.end)) release(key, h)
    }
  })
  onScopeDispose(stop)

  /** The events as shown: the one dragged at its new place. */
  function shown(events: CalendarEvent[]): CalendarEvent[] {
    const p = preview.value
    if (!p && !held.value.size) return events
    return events.map((e) => {
      const key = eventKey(e)
      const o = p?.key === key ? p : held.value.get(key)
      return o ? { ...e, start: o.start, end: o.end } : e
    })
  }

  return { start, shown, dragging, clicksBlocked: () => suppress }
}
