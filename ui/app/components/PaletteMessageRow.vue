<script setup lang="ts">
import type { MessageSummary } from '~/utils/types'

const props = defineProps<{
  message: MessageSummary
  terms: string[]
}>()
const { accounts, accountById, accountColor, ownSet } = useMail()
const { t } = useI18n()

const MARK = 'rounded px-0.5 font-semibold bg-primary-100 text-primary-800 dark:bg-primary/22 dark:text-primary-200'

const sender = computed(() => peopleLabel(props.message.from ? [props.message.from] : [], ownSet(props.message.account_id)))
const account = computed(() => (accounts.value.length > 1 ? accountById(props.message.account_id) : null))
// Escaped piece by piece in highlightHtml: safe for v-html.
const subject = computed(() => highlightHtml(props.message.subject || t('common.noSubject'), props.terms, MARK))
</script>

<template>
  <MailAvatar :address="message.from" :account-id="message.account_id" />
  <span class="flex min-w-0 flex-1 flex-col gap-0.5 text-start">
    <span class="flex min-w-0 items-center gap-2">
      <span class="truncate text-[13.5px] text-highlighted" :class="message.flags.seen ? 'font-medium' : 'font-semibold'">{{ sender }}</span>
      <span v-if="account" class="inline-flex min-w-0 items-center gap-1.5 text-[11.5px] text-muted">
        <span class="size-[7px] flex-none rounded-full" :class="accountColor(account.id).dot" />
        <span class="truncate">{{ accountName(account) }}</span>
      </span>
      <span class="ms-auto flex-none ps-2 text-xs text-muted">{{ shortDate(message.date) }}</span>
    </span>
    <span class="truncate text-[13.5px] text-default" v-html="subject" />
  </span>
  <UKbd value="enter" color="primary" class="hidden flex-none lg:group-data-highlighted:inline-flex" />
</template>
