<script setup lang="ts">
import { useEventListener } from '@vueuse/core'
/** Holds a screen other than a mail list. `floating`: a list is kept mounted underneath, and the
 *  screen covers it (the content area right of the sidebar, the whole content on mobile) until
 *  closed. Otherwise the page lays out as usual, as if this wrapper were not there. */
const props = defineProps<{ floating: boolean, inset: number }>()
const emit = defineEmits<{ close: [] }>()
const { open: composing } = useComposer()
const el = useTemplateRef<HTMLElement>('el')

/** Focus when the screen opened, given back when it closes and nothing else took it. */
const returnFocus = props.floating && document.activeElement instanceof HTMLElement ? document.activeElement : null

onMounted(() => {
  // Keyboard users land in the screen, not on the list behind it (made inert).
  if (props.floating && !el.value?.contains(document.activeElement)) el.value?.focus({ preventScroll: true })
})
onBeforeUnmount(() => {
  const node = el.value
  if (!props.floating || !returnFocus) return
  nextTick(() => {
    const active = document.activeElement
    const lost = !active || active === document.body || !!node?.contains(active)
    if (lost && returnFocus.isConnected) returnFocus.focus({ preventScroll: true })
  })
})

/** A menu, dialog or tooltip is open: Escape belongs to it. */
const layerOpen = () => !!document.querySelector('[data-dismissable-layer]')

// Escape closes the screen. Not through defineShortcuts: it prevents the event's default, and a
// dialog or menu inside the screen ignores a prevented Escape, so it would never close. Bubble
// phase: a layer that handled Escape itself has marked it prevented by then.
useEventListener(window, 'keydown', (e: KeyboardEvent) => {
  if (!props.floating || e.key !== 'Escape' || e.defaultPrevented || composing.value || layerOpen()) return
  const target = e.target as HTMLElement | null
  if (target?.closest('input, textarea, select, [contenteditable="true"]')) return
  emit('close')
})
</script>

<template>
  <!-- One element in both modes: the page is not remounted when the list underneath goes away. -->
  <div
    ref="el"
    :tabindex="floating ? -1 : undefined"
    :role="floating ? 'region' : undefined"
    :aria-label="floating ? $t('app.overlay.region') : undefined"
    :class="floating
      ? 'page-overlay absolute inset-y-0 right-0 z-30 flex bg-default outline-none animate-[fade-in_120ms_ease-out] motion-reduce:animate-none'
      : 'contents'"
    :style="floating ? { left: `${inset}px` } : undefined"
  >
    <!-- Before the page, which stays the last child (no end border). -->
    <UTooltip v-if="floating" :text="$t('app.overlay.close')" :kbds="['escape']">
      <UButton
        icon="i-lucide-x"
        color="neutral"
        variant="ghost"
        :aria-label="$t('app.overlay.close')"
        class="absolute end-4 top-4 z-10 max-lg:hidden"
        @click="emit('close')"
      />
    </UTooltip>
    <slot />
  </div>
</template>

<style scoped>
/* Room for the close button at the end of the page's navbar. */
@media (min-width: 64rem) {
  .page-overlay > :deep([data-slot='root'] > [data-slot='root']) {
    padding-inline-end: 3.75rem;
  }
}
</style>
