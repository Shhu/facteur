<script setup lang="ts">
import { useMediaQuery } from '@vueuse/core'
import { en, fr } from '@nuxt/ui/locale'
import { getCurrentWindow } from '@tauri-apps/api/window'

const { locale } = useI18n()
const uiLocale = computed(() => (locale.value === 'en' ? en : fr))

// Small screens: toasts at the top, clear of the bottom tab bar and of the reply bar.
const wide = useMediaQuery('(min-width: 1024px)')
const toaster = computed(() => ({ position: wide.value ? 'bottom-right' as const : 'top-center' as const }))

const { enabled: perfEnabled } = usePerf()

// The window opens hidden (no white flash while the page loads): shown once the app is drawn.
onMounted(() => {
  setTimeout(() => getCurrentWindow().show().catch(() => {}), 30)
})
</script>

<template>
  <UApp :locale="uiLocale" :toaster="toaster">
    <!-- The layout renders the pages itself: a mail list stays under the other screens. -->
    <NuxtLayout />
    <ConfirmDialog />
    <ConnectionAdvice />
    <DiagnosticsPanel v-if="perfEnabled" />
  </UApp>
</template>
