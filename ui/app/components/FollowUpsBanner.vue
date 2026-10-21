<script setup lang="ts">
import type { FollowUp } from '~/utils/types'

const props = defineProps<{
  /** `null`: all accounts (unified inbox). */
  accountId: number | null
}>()
const emit = defineEmits<{ open: [followUp: FollowUp] }>()

const { items, dismiss } = useFollowUps()
const list = computed(() => items.value.filter(f => props.accountId === null || f.account_id === props.accountId))
const expanded = ref(false)
const shown = computed(() => (expanded.value ? list.value : list.value.slice(0, 2)))
</script>

<template>
  <section v-if="list.length" class="border-b border-default bg-elevated/40">
    <h3 class="flex items-center gap-1.5 px-4 pt-2 text-xs font-medium text-muted sm:px-5">
      <UIcon name="i-lucide-bell-ring" class="size-3.5 text-primary" />
      {{ $t('mail.followUps.title') }}
    </h3>
    <div
      v-for="f in shown"
      :key="f.id"
      class="group flex items-center gap-2 px-4 py-1.5 sm:px-5"
      :class="f.thread_id !== null ? 'cursor-pointer hover:bg-elevated' : ''"
      @click="f.thread_id !== null && emit('open', f)"
    >
      <!-- A button when it opens (the row's click is for the pointer, this is the keyboard's). -->
      <component
        :is="f.thread_id !== null ? 'button' : 'div'"
        :type="f.thread_id !== null ? 'button' : undefined"
        class="block min-w-0 flex-1 text-start"
        :class="f.thread_id !== null && 'cursor-pointer rounded-sm'"
        @click.stop="f.thread_id !== null && emit('open', f)"
      >
        <span class="block truncate text-sm text-highlighted">{{ f.subject || $t('common.noSubject') }}</span>
        <span class="block truncate text-xs text-muted">{{ $t('common.sentTo', { to: f.to.join(', ') || '…', ago: ago(f.sent_at) }) }}</span>
      </component>
      <UTooltip :text="$t('mail.followUps.dismiss')">
        <UButton
          icon="i-lucide-x"
          size="xs"
          color="neutral"
          variant="ghost"
          :aria-label="$t('mail.followUps.dismiss')"
          @click.stop="dismiss(f.id)"
        />
      </UTooltip>
    </div>
    <UButton
      v-if="list.length > 2"
      :label="expanded ? $t('mail.followUps.less') : $t('mail.followUps.more', { n: list.length - 2 })"
      size="xs"
      color="neutral"
      variant="link"
      class="mb-1 ms-2 sm:ms-3"
      @click="expanded = !expanded"
    />
  </section>
</template>
