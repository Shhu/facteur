<script setup lang="ts">
/** Returns a data URL, or the original image untouched when nothing changed. */
const open = defineModel<boolean>('open', { required: true })
const props = defineProps<{ src: string, /** Shape picked when an image loads. */ initialShape?: Shape }>()
const emit = defineEmits<{ done: [src: string] }>()
const { t } = useI18n()
const showError = useErrorToast()

/** In pixels of the original image. */
interface Rect { x: number, y: number, w: number, h: number }
type Shape = 'free' | 'square' | 'circle' | '4:3' | '16:9'

const img = useTemplateRef<HTMLImageElement>('img')
const natural = ref({ w: 0, h: 0 })
const crop = ref<Rect>({ x: 0, y: 0, w: 0, h: 0 })
const shape = ref<Shape>('free')
const MIN = 16

const ratio = computed(() => ({ 'free': null, 'square': 1, 'circle': 1, '4:3': 4 / 3, '16:9': 16 / 9 })[shape.value])
const shapes = computed(() => [
  { value: 'free' as const, label: t('app.cropper.free'), icon: 'i-lucide-crop' },
  { value: 'square' as const, label: t('app.cropper.square'), icon: 'i-lucide-square' },
  { value: 'circle' as const, label: t('app.cropper.circle'), icon: 'i-lucide-circle' },
  { value: '4:3' as const, label: '4:3', icon: 'i-lucide-rectangle-horizontal' },
  { value: '16:9' as const, label: '16:9', icon: 'i-lucide-rectangle-horizontal' }
])

function onLoad() {
  const el = img.value
  if (!el) return
  natural.value = { w: el.naturalWidth, h: el.naturalHeight }
  shape.value = 'free'
  crop.value = { x: 0, y: 0, w: el.naturalWidth, h: el.naturalHeight }
  if (props.initialShape && props.initialShape !== 'free') setShape(props.initialShape)
}

/** Largest rectangle of the new shape's ratio, centred on the current frame. */
function setShape(next: Shape) {
  shape.value = next
  const r = ratio.value
  if (!r) return
  const { w: nw, h: nh } = natural.value
  const c = crop.value
  let w = Math.min(c.w, nw)
  let h = w / r
  if (h > nh) {
    h = nh
    w = h * r
  }
  const cx = c.x + c.w / 2
  const cy = c.y + c.h / 2
  crop.value = { x: clamp(cx - w / 2, 0, nw - w), y: clamp(cy - h / 2, 0, nh - h), w, h }
}

const clamp = (v: number, min: number, max: number) => Math.min(Math.max(v, min), max)

type Edge = 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw'
type Handle = 'move' | Edge
const handles = computed<Edge[]>(() => ratio.value ? ['nw', 'ne', 'sw', 'se'] : ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'])
let drag: { handle: Handle, x: number, y: number, start: Rect, scale: number } | null = null

function onPointerDown(e: PointerEvent, handle: Handle) {
  const el = img.value
  if (!el) return
  e.preventDefault()
  ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
  drag = { handle, x: e.clientX, y: e.clientY, start: { ...crop.value }, scale: natural.value.w / el.getBoundingClientRect().width }
}

function onPointerMove(e: PointerEvent) {
  if (!drag) return
  const dx = (e.clientX - drag.x) * drag.scale
  const dy = (e.clientY - drag.y) * drag.scale
  const { w: nw, h: nh } = natural.value
  const s = drag.start
  if (drag.handle === 'move') {
    crop.value = { ...s, x: clamp(s.x + dx, 0, nw - s.w), y: clamp(s.y + dy, 0, nh - s.h) }
    return
  }
  const dir = drag.handle
  let left = s.x
  let top = s.y
  let right = s.x + s.w
  let bottom = s.y + s.h
  if (dir.includes('w')) left = clamp(s.x + dx, 0, right - MIN)
  if (dir.includes('e')) right = clamp(right + dx, left + MIN, nw)
  if (dir.includes('n')) top = clamp(s.y + dy, 0, bottom - MIN)
  if (dir.includes('s')) bottom = clamp(bottom + dy, top + MIN, nh)
  const r = ratio.value
  if (r) {
    // Width leads; height follows from the opposite edge, staying within the image.
    let w = right - left
    let h = w / r
    const maxH = dir.includes('n') ? s.y + s.h : nh - s.y
    if (h > maxH) {
      h = maxH
      w = h * r
    }
    if (dir.includes('w')) left = right - w
    else right = left + w
    if (dir.includes('n')) top = bottom - h
    else bottom = top + h
  }
  crop.value = { x: left, y: top, w: right - left, h: bottom - top }
}

/** End of drag, or pointer lost (`pointercancel`: the system took over the gesture). */
function onPointerUp() {
  drag = null
}

/** Frame position as percentages of the displayed image. */
const box = computed(() => {
  const { w: nw, h: nh } = natural.value
  if (!nw || !nh) return {}
  const c = crop.value
  return { left: `${c.x / nw * 100}%`, top: `${c.y / nh * 100}%`, width: `${c.w / nw * 100}%`, height: `${c.h / nh * 100}%` }
})

const handleClass: Record<Edge, string> = {
  nw: '-left-1.5 -top-1.5 cursor-nwse-resize',
  n: 'left-1/2 -top-1.5 -translate-x-1/2 cursor-ns-resize',
  ne: '-right-1.5 -top-1.5 cursor-nesw-resize',
  e: '-right-1.5 top-1/2 -translate-y-1/2 cursor-ew-resize',
  se: '-right-1.5 -bottom-1.5 cursor-nwse-resize',
  s: 'left-1/2 -bottom-1.5 -translate-x-1/2 cursor-ns-resize',
  sw: '-left-1.5 -bottom-1.5 cursor-nesw-resize',
  w: '-left-1.5 top-1/2 -translate-y-1/2 cursor-ew-resize'
}

function trim() {
  const el = img.value
  if (!el) return
  const { w: nw, h: nh } = natural.value
  // Analysed on a downscaled copy: precise enough, and fast even for a photo.
  const scale = Math.min(1, 800 / Math.max(nw, nh))
  const w = Math.max(1, Math.round(nw * scale))
  const h = Math.max(1, Math.round(nh * scale))
  const canvas = document.createElement('canvas')
  canvas.width = w
  canvas.height = h
  const ctx = canvas.getContext('2d', { willReadFrequently: true })
  if (!ctx) return
  let data: Uint8ClampedArray
  try {
    ctx.drawImage(el, 0, 0, w, h)
    data = ctx.getImageData(0, 0, w, h).data
  } catch (e) {
    // Cross-origin image: the tainted canvas cannot be read (SecurityError).
    showError(e, t('app.cropper.failed'))
    return
  }
  const bg = [data[0]!, data[1]!, data[2]!, data[3]!]
  const differs = (i: number) => bg[3]! < 16
    ? data[i + 3]! > 24
    : Math.abs(data[i]! - bg[0]!) + Math.abs(data[i + 1]! - bg[1]!) + Math.abs(data[i + 2]! - bg[2]!) > 48 || data[i + 3]! < bg[3]! - 24
  let minX = w
  let minY = h
  let maxX = -1
  let maxY = -1
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      if (!differs((y * w + x) * 4)) continue
      if (x < minX) minX = x
      if (x > maxX) maxX = x
      if (y < minY) minY = y
      if (y > maxY) maxY = y
    }
  }
  if (maxX < 0) return
  shape.value = 'free'
  const x = Math.max(0, Math.floor(minX / scale) - 2)
  const y = Math.max(0, Math.floor(minY / scale) - 2)
  crop.value = { x, y, w: Math.min(nw, Math.ceil((maxX + 1) / scale) + 2) - x, h: Math.min(nh, Math.ceil((maxY + 1) / scale) + 2) - y }
}

function apply() {
  const el = img.value
  const c = crop.value
  const { w: nw, h: nh } = natural.value
  const whole = Math.round(c.x) === 0 && Math.round(c.y) === 0 && Math.round(c.w) === nw && Math.round(c.h) === nh
  if (!el || (whole && shape.value !== 'circle')) {
    emit('done', props.src)
    open.value = false
    return
  }
  const w = Math.max(1, Math.round(c.w))
  const h = Math.max(1, Math.round(c.h))
  const canvas = document.createElement('canvas')
  canvas.width = w
  canvas.height = h
  const ctx = canvas.getContext('2d')
  if (!ctx) return
  if (shape.value === 'circle') {
    ctx.beginPath()
    ctx.arc(w / 2, h / 2, Math.min(w, h) / 2, 0, Math.PI * 2)
    ctx.clip()
  }
  // JPEG for photos; PNG keeps transparency (logos, circle).
  const jpeg = props.src.startsWith('data:image/jpeg') && shape.value !== 'circle'
  let result: string
  try {
    ctx.drawImage(el, Math.round(c.x), Math.round(c.y), w, h, 0, 0, w, h)
    result = jpeg ? canvas.toDataURL('image/jpeg', 0.92) : canvas.toDataURL('image/png')
  } catch (e) {
    // Cross-origin image: the tainted canvas cannot be exported (SecurityError).
    showError(e, t('app.cropper.failed'))
    return
  }
  emit('done', result)
  open.value = false
}
</script>

<template>
  <UModal v-model:open="open" :title="$t('app.cropper.title')" :ui="{ content: 'max-w-3xl' }">
    <template #body>
      <div class="flex flex-wrap items-center gap-2">
        <UFieldGroup>
          <UButton
            v-for="s in shapes"
            :key="s.value"
            :label="s.label"
            :icon="s.icon"
            size="sm"
            :color="shape === s.value ? 'primary' : 'neutral'"
            :variant="shape === s.value ? 'soft' : 'outline'"
            @click="setShape(s.value)"
          />
        </UFieldGroup>
        <UButton
          :label="$t('app.cropper.trim')"
          icon="i-lucide-scan"
          size="sm"
          color="neutral"
          variant="ghost"
          class="ms-auto"
          @click="trim"
        />
      </div>
      <!-- Checkerboard: shows where the image is transparent. -->
      <div class="mt-4 flex justify-center rounded-md bg-[repeating-conic-gradient(#8881_0_25%,transparent_0_50%)] bg-size-[16px_16px] p-4">
        <div class="relative inline-block touch-none select-none overflow-hidden" @pointermove="onPointerMove" @pointerup="onPointerUp" @pointercancel="onPointerUp">
          <img ref="img" :src="src" alt="" class="block max-h-[55vh] max-w-full" draggable="false" @load="onLoad">
          <div
            class="absolute cursor-move border-2 border-white shadow-[0_0_0_9999px_rgb(0_0_0/0.55)]"
            :class="shape === 'circle' ? 'rounded-full' : ''"
            :style="box"
            @pointerdown="onPointerDown($event, 'move')"
          >
            <span
              v-for="h in handles"
              :key="h"
              class="absolute size-3 rounded-sm border border-neutral-500 bg-white"
              :class="handleClass[h]"
              @pointerdown.stop="onPointerDown($event, h)"
            />
          </div>
        </div>
      </div>
      <p class="mt-2 text-center text-xs text-muted">
        {{ Math.round(crop.w) }} × {{ Math.round(crop.h) }} px
      </p>
    </template>
    <template #footer>
      <div class="flex w-full justify-end gap-2">
        <UButton :label="$t('common.cancel')" color="neutral" variant="ghost" @click="open = false" />
        <UButton :label="$t('app.cropper.apply')" icon="i-lucide-check" @click="apply" />
      </div>
    </template>
  </UModal>
</template>
