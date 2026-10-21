<script setup lang="ts">
/** `list`: the mobile settings list (no updates there: the Android app is installed by hand).
 *  Only the Windows exe updates itself. */
defineProps<{ layout: 'card' | 'list' }>()
const { supported, version, available, checking, installing, progress, checkNow, install } = useUpdater()
const showError = useErrorToast()

const status = computed(() => {
  if (installing.value) return progress.value === null ? tr('settings.about.installing') : tr('settings.about.downloading', { percent: Math.round(progress.value * 100) })
  if (available.value) return tr('settings.about.availableTitle', { version: available.value.version })
  return tr('settings.about.current')
})
</script>

<template>
  <template v-if="layout === 'card'">
    <SettingsRow :label="$t('settings.about.version', { version: version ?? '…' })" :description="supported ? status : undefined">
      <div class="flex items-center gap-2">
        <UButton
          :label="$t('settings.about.logs')"
          icon="i-lucide-scroll-text"
          color="neutral"
          variant="ghost"
          class="h-10 rounded-[10px] px-3"
          @click="api.openLogs().catch(showError)"
        />
        <UButton
          v-if="supported && available"
          :label="$t('settings.about.install')"
          icon="i-lucide-download"
          class="h-10 rounded-[10px] px-4"
          :loading="installing"
          @click="install"
        />
        <UButton
          v-else-if="supported"
          :label="$t('settings.about.check')"
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="outline"
          class="h-10 rounded-[10px] bg-(--surface-card) px-4"
          :loading="checking"
          @click="checkNow(true)"
        />
      </div>
    </SettingsRow>
  </template>
  <SettingsListRow
    v-else
    :label="$t('settings.about.title')"
    icon="i-lucide-info"
    tone="bg-zinc-500"
    :value="version ? `facteur ${version}` : undefined"
  />
</template>
