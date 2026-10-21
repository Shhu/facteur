<script setup lang="ts">
const props = defineProps<{ open: boolean, from: string, excerpt: string }>()
defineEmits<{ toggle: [] }>()
const { t } = useI18n()

/** French elides « de » before a vowel: « d'Outlook ». */
const label = computed(() => (props.open
  ? t('mail.invite.hideOriginal')
  : /^[aeiouyhàâäéèêëîïôöûü]/i.test(props.from) ? t('mail.invite.showOriginalElided', { from: props.from }) : t('mail.invite.showOriginal', { from: props.from })))
</script>

<template>
  <button
    type="button"
    class="flex min-h-12 w-full items-center gap-2.5 rounded-[14px] border border-dashed border-default px-3.5 text-left text-[13.5px] text-muted transition-colors hover:bg-elevated/40 hover:text-toned lg:min-h-11 lg:rounded-xl lg:text-[13px]"
    :aria-expanded="open"
    @click="$emit('toggle')"
  >
    <UIcon :name="open ? 'i-lucide-chevron-up' : 'i-lucide-chevron-down'" class="size-4 shrink-0" />
    <span class="min-w-0 flex-1 truncate">{{ label }}</span>
    <span v-if="!open && excerpt" class="hidden max-w-[45%] shrink truncate text-xs text-dimmed lg:block">{{ $t('mail.invite.excerpt', { text: excerpt }) }}</span>
  </button>
</template>
