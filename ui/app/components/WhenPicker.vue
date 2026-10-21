<script setup lang="ts">
import type { WhenPreset } from '~/utils/format'

const { request, answer } = useWhenPicker()

const presets = ref<WhenPreset[]>([])
const custom = ref('')

/** Value for a `datetime-local` input (local time, no zone). */
function localInput(ts: number) {
  const d = new Date(ts * 1000)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

watch(request, (r) => {
  if (!r) return
  presets.value = whenPresets(r.kind)
  custom.value = localInput(presets.value[0]!.at)
})

const customAt = computed(() => {
  const t = new Date(custom.value).getTime()
  return Number.isNaN(t) ? null : Math.floor(t / 1000)
})
const customValid = computed(() => customAt.value !== null && customAt.value > Date.now() / 1000)
</script>

<template>
  <UModal
    :open="request !== null"
    :title="request?.title"
    :ui="{ content: 'max-w-sm', body: 'p-2 sm:p-2' }"
    @update:open="(o) => { if (!o) answer(null) }"
  >
    <template #body>
      <div class="flex flex-col">
        <button
          v-for="p in presets"
          :key="p.label"
          type="button"
          class="flex items-center gap-3 rounded-md px-3 py-2 text-left text-sm hover:bg-elevated focus-visible:bg-elevated focus-visible:outline-none"
          @click="answer(p.at)"
        >
          <UIcon :name="p.icon" class="size-4 shrink-0 text-muted" />
          <span class="flex-1 text-highlighted">{{ p.label }}</span>
          <span class="text-xs text-muted">{{ untilLabel(p.at) }}</span>
        </button>
        <form class="mt-2 flex items-center gap-2 border-t border-default px-1 pt-3" @submit.prevent="customValid && answer(customAt)">
          <UInput v-model="custom" type="datetime-local" size="sm" class="flex-1" :aria-label="$t('app.when.dateTime')" />
          <UButton type="submit" :label="$t('app.when.choose')" size="sm" :disabled="!customValid" />
        </form>
      </div>
    </template>
  </UModal>
</template>
