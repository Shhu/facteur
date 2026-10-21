<script setup lang="ts">
import type { Mailbox, MessageSummary } from '~/utils/types'
import { folderLabel } from '~/components/SimilarFolderMenu.vue'

const checked = defineModel<boolean>({ required: true })
const props = defineProps<{
  message: MessageSummary
  folder: Mailbox | undefined
  touch?: boolean
}>()
const { t } = useI18n()

const subject = computed(() => props.message.subject || t('common.noSubject'))
const meta = computed(() => [addressLabel(props.message.from), shortDate(props.message.date)].filter(Boolean).join(' · '))
const tag = computed(() => (props.folder?.role === 'inbox' ? 'bg-primary/14 text-primary' : 'bg-elevated text-toned'))
</script>

<template>
  <div
    class="flex cursor-default items-center transition-colors"
    :class="[
      touch ? 'min-h-15 gap-3 px-3.5 py-2' : 'gap-3 rounded-[10px] px-2.5 py-2.5',
      checked ? 'bg-primary/5 dark:bg-primary/6' : touch ? '' : 'hover:bg-elevated/50'
    ]"
    @click="checked = !checked"
  >
    <SimilarCheck v-model="checked" :touch="touch" :label="subject" />
    <MailAvatar v-if="!touch" :address="message.from" :account-id="message.account_id" class="size-8.5!" />
    <div class="flex min-w-0 flex-1 flex-col gap-px" :class="touch ? 'ps-0.5' : ''">
      <span class="truncate text-sm text-highlighted">{{ subject }}</span>
      <span class="flex min-w-0 items-center gap-1.5 text-muted" :class="touch ? 'text-xs' : 'text-[12.5px]'">
        <span class="truncate">{{ meta }}</span>
        <span
          v-if="touch && folder"
          class="inline-flex shrink-0 items-center gap-1.25 rounded-full px-1.75 py-px font-semibold"
          :class="tag"
        >{{ folderLabel(folder) }}</span>
      </span>
    </div>
    <span
      v-if="!touch && folder"
      class="inline-flex max-w-40 shrink-0 items-center gap-1.5 rounded-full px-2.25 py-0.75 text-xs font-semibold"
      :class="tag"
    >
      <span class="size-1.5 shrink-0 rounded-full bg-current" />
      <span class="truncate">{{ folderLabel(folder) }}</span>
    </span>
  </div>
</template>
