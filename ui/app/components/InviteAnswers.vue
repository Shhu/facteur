<script setup lang="ts">
export type Partstat = 'ACCEPTED' | 'TENTATIVE' | 'DECLINED'

defineProps<{ response: string | null, sending: Partstat | null }>()
const emit = defineEmits<{ respond: [partstat: Partstat] }>()
const { t } = useI18n()

const answers = computed(() => [
  { value: 'ACCEPTED' as const, label: t('mail.invite.yes'), icon: 'i-lucide-check', on: 'border-green-700 bg-green-700 text-white', tint: 'text-green-700 dark:text-green-400' },
  { value: 'TENTATIVE' as const, label: t('mail.invite.maybe'), icon: 'i-lucide-circle-help', on: 'border-amber-700 bg-amber-700 text-white', tint: 'text-amber-700 dark:text-amber-400' },
  { value: 'DECLINED' as const, label: t('mail.invite.no'), icon: 'i-lucide-x', on: 'border-red-600 bg-red-600 text-white', tint: 'text-red-600 dark:text-red-400' }
])
</script>

<template>
  <div role="radiogroup" :aria-label="$t('mail.invite.yourResponse')" class="grid grid-cols-3 gap-2 lg:flex lg:gap-1.5">
    <button
      v-for="a in answers"
      :key="a.value"
      type="button"
      role="radio"
      :aria-checked="response === a.value"
      :disabled="sending !== null"
      class="flex h-12 min-w-0 items-center justify-center gap-1.5 rounded-xl border text-sm font-semibold transition-colors disabled:cursor-default lg:h-9.5 lg:gap-1.75 lg:rounded-[10px] lg:px-3.5 lg:text-[13.5px]"
      :class="response === a.value ? a.on : 'border-default bg-(--surface-card) text-highlighted enabled:hover:bg-elevated/60 lg:bg-transparent'"
      @click="response !== a.value && emit('respond', a.value)"
    >
      <UIcon
        :name="sending === a.value ? 'i-lucide-loader-circle' : a.icon"
        class="size-4 shrink-0"
        :class="[sending === a.value && 'animate-spin', response === a.value ? 'text-white' : a.tint]"
      />
      <span class="truncate">{{ a.label }}</span>
    </button>
  </div>
</template>
