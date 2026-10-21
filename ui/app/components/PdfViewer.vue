<script setup lang="ts">
import { useResizeObserver } from '@vueuse/core'
import * as pdfjs from 'pdfjs-dist'
import { TextLayer } from 'pdfjs-dist'
import type { PDFDocumentLoadingTask, PDFDocumentProxy, PDFPageProxy, RenderTask } from 'pdfjs-dist'
import workerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url'

/**
 * Drawn by pdf.js into canvases: no system viewer, no document script runs (`isEvalSupported:
 * false`, no form or link layer). A transparent text layer on each page makes text selectable.
 * Pages draw when they come into view and give their canvas back when they leave.
 */
const props = defineProps<{ data: ArrayBuffer }>()

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl

const doc = shallowRef<PDFDocumentProxy | null>(null)
const error = ref<string | null>(null)
/** Height/width ratio of each page, learned as they draw; the first one serves as the estimate. */
const ratios = ref<number[]>([])
const zoom = ref(1)
const ZOOM_MIN = 0.5
const ZOOM_MAX = 3

/** Zooms keeping the point under the mouse (or the view's centre) still. */
async function zoomBy(step: number, e?: MouseEvent) {
  const el = container.value
  const next = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Math.round((zoom.value + step) * 100) / 100))
  if (!el || next === zoom.value) return
  const rect = el.getBoundingClientRect()
  const px = e ? e.clientX - rect.left : el.clientWidth / 2
  const py = e ? e.clientY - rect.top : el.clientHeight / 2
  // The point's position in the document, as a fraction of its current size.
  const fx = (el.scrollLeft + px) / el.scrollWidth
  const fy = (el.scrollTop + py) / el.scrollHeight
  zoom.value = next
  await nextTick()
  el.scrollLeft = fx * el.scrollWidth - px
  el.scrollTop = fy * el.scrollHeight - py
}

/** Ctrl + wheel: zoom (without scrolling or zooming the whole window). */
function onWheel(e: WheelEvent) {
  if (!e.ctrlKey && !e.metaKey) return
  e.preventDefault()
  zoomBy(e.deltaY < 0 ? 0.1 : -0.1, e)
}
const container = useTemplateRef<HTMLElement>('container')
const width = ref(800)
useResizeObserver(container, ([entry]) => {
  if (entry) width.value = Math.max(200, entry.contentRect.width - 32)
})
const pageWidth = computed(() => Math.round(width.value * zoom.value))

/** Kept to abort loading if the viewer closes first. */
let loading: PDFDocumentLoadingTask | null = null
/** Viewer closed: whatever an await returns must touch nothing. */
let unmounted = false

onMounted(async () => {
  try {
    // Copy: pdf.js transfers the buffer to its worker.
    loading = pdfjs.getDocument({
      data: new Uint8Array(props.data.slice(0)),
      isEvalSupported: false,
      enableXfa: false,
      useSystemFonts: true,
      stopAtErrors: false
    })
    const d = await loading.promise
    if (unmounted) return
    const first = (await d.getPage(1)).getViewport({ scale: 1 })
    if (unmounted) return
    ratios.value = Array.from({ length: d.numPages }, () => first.height / first.width)
    doc.value = d
    loading = null
  } catch (e) {
    if (!unmounted) error.value = String(e)
  }
})
onBeforeUnmount(() => {
  unmounted = true
  observer?.disconnect()
  for (const r of renders.values()) r.cancel()
  // Still loading: destroying the task also stops the worker.
  if (loading) loading.destroy()
  else doc.value?.destroy()
})

const pages = new Map<number, HTMLElement>()
const pageOf = new WeakMap<Element, number>()
/** Width each page was drawn at (absent: not yet, or released). */
const drawn = new Map<number, number>()
const visible = reactive(new Set<number>())
/** Latest draw started per page: a newer draw, or leaving the view, makes it stale. */
const current = new Map<number, number>()
let drawCount = 0
const renders = new Map<number, RenderTask>()
/** Pages loaded by pdf.js: their resources are freed when they leave the view. */
const loadedPages = new Map<number, PDFPageProxy>()

async function draw(n: number) {
  const d = doc.value
  const frame = pages.get(n)
  const canvas = frame?.querySelector('canvas')
  const text = frame?.querySelector<HTMLElement>('.textLayer')
  if (unmounted || !d || !canvas || !text || drawn.get(n) === pageWidth.value) return
  const cssWidth = pageWidth.value
  const token = ++drawCount
  current.set(n, token)
  drawn.set(n, cssWidth)
  renders.get(n)?.cancel()
  const stale = () => unmounted || current.get(n) !== token
  try {
    const page = await d.getPage(n)
    if (stale()) return
    loadedPages.set(n, page)
    const base = page.getViewport({ scale: 1 })
    ratios.value[n - 1] = base.height / base.width
    const dpr = window.devicePixelRatio || 1
    const viewport = page.getViewport({ scale: (cssWidth / base.width) * dpr })
    canvas.width = Math.floor(viewport.width)
    canvas.height = Math.floor(viewport.height)
    const ctx = canvas.getContext('2d')
    if (ctx) {
      const task = page.render({ canvasContext: ctx, viewport })
      renders.set(n, task)
      await task.promise
      if (stale()) return
    }
    const scale = cssWidth / base.width
    text.replaceChildren()
    text.style.setProperty('--scale-factor', String(scale))
    await new TextLayer({ textContentSource: page.streamTextContent(), container: text, viewport: page.getViewport({ scale }) })
      .render()
      .catch(() => {})
  } catch {
    // Cancelled or failed: the page is drawn again next time it comes into view.
    if (!stale()) drawn.delete(n)
  } finally {
    if (current.get(n) === token) renders.delete(n)
  }
}

/** A page left the view: its canvas (megabytes at high zoom) goes back to the system. */
function release(n: number) {
  current.delete(n)
  drawn.delete(n)
  renders.get(n)?.cancel()
  renders.delete(n)
  const frame = pages.get(n)
  const canvas = frame?.querySelector('canvas')
  if (canvas) canvas.width = canvas.height = 0
  frame?.querySelector('.textLayer')?.replaceChildren()
  loadedPages.get(n)?.cleanup()
  loadedPages.delete(n)
}

watch([pageWidth, () => [...visible]], () => {
  for (const n of visible) draw(n)
})

/** One observer for all pages, created with the first (the container exists by then) and
 *  disconnected on unmount: a template ref has no scope that would do it. */
let observer: IntersectionObserver | null = null

function onIntersect(entries: IntersectionObserverEntry[]) {
  for (const entry of entries) {
    const n = pageOf.get(entry.target)
    if (n === undefined) continue
    if (entry.isIntersecting) {
      visible.add(n)
      draw(n)
    } else if (visible.has(n)) {
      visible.delete(n)
      release(n)
    }
  }
}

function observe(n: number, el: unknown) {
  if (unmounted || !(el instanceof HTMLElement) || pages.get(n) === el) return
  observer ??= new IntersectionObserver(onIntersect, { root: container.value, rootMargin: '600px' })
  const before = pages.get(n)
  if (before) observer.unobserve(before)
  pages.set(n, el)
  pageOf.set(el, n)
  drawn.delete(n)
  observer.observe(el)
}
</script>

<template>
  <div class="flex size-full flex-col">
    <div class="flex items-center justify-center gap-1 border-b border-default bg-default px-2 py-1 text-xs text-muted">
      <UButton icon="i-lucide-zoom-out" size="xs" color="neutral" variant="ghost" :aria-label="$t('mail.pdf.zoomOut')" :disabled="zoom <= ZOOM_MIN" @click="zoomBy(-0.25)" />
      <span class="w-12 text-center tabular-nums">{{ $t('mail.pdf.percent', { n: Math.round(zoom * 100) }) }}</span>
      <UButton icon="i-lucide-zoom-in" size="xs" color="neutral" variant="ghost" :aria-label="$t('mail.pdf.zoomIn')" :disabled="zoom >= ZOOM_MAX" @click="zoomBy(0.25)" />
      <UButton :label="$t('mail.pdf.fitWidth')" size="xs" color="neutral" variant="ghost" :disabled="zoom === 1" @click="zoom = 1" />
      <span v-if="doc" class="ms-3">{{ $t('mail.pdf.pages', doc.numPages) }}</span>
    </div>
    <div ref="container" class="min-h-0 flex-1 overflow-auto p-4" @wheel="onWheel">
      <p v-if="error" class="py-10 text-center text-sm text-muted">{{ $t('mail.pdf.unreadable', { error }) }}</p>
      <div v-else-if="!doc" class="flex items-center justify-center gap-2 py-10 text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" /> {{ $t('mail.pdf.opening') }}
      </div>
      <div v-else class="mx-auto flex flex-col items-center gap-4" :style="{ width: `${pageWidth}px` }">
        <div
          v-for="n in doc.numPages"
          :key="n"
          :ref="(el: unknown) => observe(n, el)"
          class="relative rounded-sm bg-white shadow"
          :style="{ width: `${pageWidth}px`, height: `${Math.round(pageWidth * (ratios[n - 1] ?? 1.414))}px` }"
        >
          <canvas class="block size-full" :aria-label="$t('mail.pdf.page', { n })" />
          <div class="textLayer" />
        </div>
      </div>
    </div>
  </div>
</template>

<style>
/* pdf.js text layer (from pdf_viewer.css): transparent, selectable text. */
.textLayer {
  position: absolute;
  inset: 0;
  overflow: clip;
  line-height: 1;
  text-size-adjust: none;
  forced-color-adjust: none;
  transform-origin: 0 0;
  z-index: 0;
}
.textLayer :is(span, br) {
  color: transparent;
  position: absolute;
  white-space: pre;
  cursor: text;
  transform-origin: 0% 0%;
}
.textLayer span.markedContent {
  top: 0;
  height: 0;
}
.textLayer ::selection {
  background: rgb(0 0 255 / 0.25);
}
.textLayer br::selection {
  background: transparent;
}
.textLayer .endOfContent {
  display: block;
  position: absolute;
  inset: 100% 0 0;
  z-index: 0;
  cursor: default;
  user-select: none;
}
</style>
