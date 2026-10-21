<script lang="ts">
import type { Mailbox } from '~/utils/types'

/** Folders messages can be moved to: not Pinned (a flag, not a place), Drafts, Later (needs a
 *  date) nor the containers that cannot hold messages. Special folders first, user folders after. */
export function moveTargets(mailboxes: Mailbox[], accountId: number): Mailbox[] {
  const order: Partial<Record<NonNullable<Mailbox['role']>, number>> = { inbox: 0, all: 1, archive: 2, sent: 3, junk: 5, trash: 6 }
  const rank = (m: Mailbox) => (m.role ? order[m.role] ?? 9 : isImportant(m) ? 4 : 7)
  return mailboxes
    .filter(m => m.account_id === accountId && m.selectable && m.role !== 'flagged' && m.role !== 'drafts' && m.role !== 'snoozed')
    .sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name))
}

/** Gmail's "Important" label is an ordinary folder over IMAP: it has no role of its own. */
function isImportant(m: Mailbox): boolean {
  return !m.role && /(^|[/.])important$/i.test(m.name)
}

export function folderLabel(m: Mailbox): string {
  return m.role ? roleLabels[m.role] : m.display_name
}

/** Icon and tile colours of a destination. Full class strings so that Tailwind generates them. */
export function folderLook(m: Mailbox): { icon: string, tone: string } {
  if (m.role === 'inbox') return { icon: roleIcons.inbox, tone: 'bg-primary/14 text-primary' }
  if (m.role === 'sent') return { icon: roleIcons.sent, tone: 'bg-sky-100 text-sky-700 dark:bg-sky-500/16 dark:text-sky-400' }
  if (m.role === 'trash') return { icon: roleIcons.trash, tone: 'bg-red-100 text-red-700 dark:bg-red-500/14 dark:text-red-300' }
  if (isImportant(m)) return { icon: 'i-lucide-bookmark', tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/16 dark:text-amber-400' }
  return { icon: m.role ? roleIcons[m.role] : 'i-lucide-folder', tone: 'bg-elevated text-toned' }
}
</script>

<script setup lang="ts">
const model = defineModel<number | null>({ required: true })
const props = withDefaults(defineProps<{
  accountId: number
  size?: 'md' | 'lg'
}>(), { size: 'md' })
const { t } = useI18n()
const { mailboxes, accountById, accountColor, loadMailboxes } = useMail()
const showError = useErrorToast()

const items = computed(() => moveTargets(mailboxes.value, props.accountId).map(m => ({
  value: m.id,
  label: folderLabel(m),
  ...folderLook(m)
})))
/** The menu wants `undefined` for no choice. */
const selected = computed({
  get: () => model.value ?? undefined,
  set: (id: number | undefined) => {
    model.value = id ?? null
  }
})
const current = computed(() => items.value.find(i => i.value === model.value))
const account = computed(() => accountById(props.accountId))
const open = ref(false)

const naming = ref(false)
const name = ref('')
const creating = ref(false)

function startNewFolder() {
  open.value = false
  name.value = ''
  naming.value = true
}

async function createFolder() {
  const n = name.value.trim()
  if (!n || creating.value) return
  creating.value = true
  try {
    const id = await api.createFolder(props.accountId, null, n)
    await loadMailboxes([props.accountId])
    model.value = id
    naming.value = false
  } catch (e) {
    showError(e)
  } finally {
    creating.value = false
  }
}
</script>

<template>
  <USelectMenu
    v-model="selected"
    v-model:open="open"
    :items="items"
    value-key="value"
    :search-input="items.length > 12 ? { placeholder: t('tools.cleanup.searchFolder'), icon: 'i-lucide-search' } : false"
    :content="{ side: 'top', align: 'start', sideOffset: 8 }"
    :ui="{
      // Leading and trailing in the flow (not laid over the text): the button keeps its own padding.
      base: size === 'lg' ? 'h-12 w-full gap-2.5 rounded-xl px-3.5' : 'h-10.5 w-60 gap-2.5 rounded-[10px] px-3',
      leading: 'static ps-0',
      trailing: 'static pe-0',
      content: 'max-h-[min(26rem,var(--reka-combobox-content-available-height,26rem))] min-w-70 rounded-[14px] p-1.5',
      viewport: 'p-0',
      item: 'h-9.5 items-center px-2.5 before:rounded-[9px] data-highlighted:not-data-disabled:before:bg-elevated data-[state=checked]:before:bg-primary/12',
      trailingIcon: 'text-muted'
    }"
    :aria-label="t('tools.cleanup.folderMenu')"
    trailing-icon="i-lucide-chevron-down"
  >
    <template #leading>
      <span
        v-if="size === 'lg'"
        class="flex size-7 shrink-0 items-center justify-center rounded-lg"
        :class="current ? current.tone : 'bg-primary/14 text-primary'"
      >
        <UIcon :name="current ? current.icon : 'i-lucide-folder'" class="size-4" />
      </span>
      <UIcon v-else name="i-lucide-folder" class="size-4 shrink-0 text-primary" />
    </template>
    <template #default>
      <span v-if="size === 'lg'" class="flex min-w-0 flex-1 flex-col text-start leading-tight">
        <span class="text-xs text-muted">{{ t('tools.cleanup.toFolder') }}</span>
        <span class="truncate text-[15px] font-semibold" :class="current ? 'text-highlighted' : 'text-muted'">
          {{ current?.label ?? t('tools.cleanup.chooseFolder') }}
        </span>
      </span>
      <span v-else class="min-w-0 flex-1 truncate text-start text-[13.5px] font-medium" :class="current ? 'text-highlighted' : 'text-muted'">
        {{ current?.label ?? t('tools.cleanup.chooseFolder') }}
      </span>
    </template>
    <template #content-top>
      <div v-if="account" class="flex items-center gap-2 px-2.5 pt-2 pb-1.5 text-[11.5px] font-bold uppercase tracking-[0.05em] text-muted">
        <span class="size-2 shrink-0 rounded-full" :class="accountColor(account.id).dot" />
        <span class="truncate">{{ accountName(account) }}</span>
      </div>
    </template>
    <template #item="{ item }">
      <span class="flex w-full min-w-0 items-center gap-2.5">
        <span class="flex size-6.5 shrink-0 items-center justify-center rounded-lg" :class="item.tone">
          <UIcon :name="item.icon" class="size-3.5" />
        </span>
        <span class="min-w-0 flex-1 truncate text-[13.5px] text-highlighted">{{ item.label }}</span>
        <UIcon v-if="item.value === model" name="i-lucide-check" class="size-4 shrink-0 text-primary" />
      </span>
    </template>
    <template #content-bottom>
      <div class="mx-1 my-1.5 h-px bg-(--ui-border)" />
      <button
        type="button"
        class="flex h-9.5 w-full items-center gap-2.5 rounded-[9px] px-2.5 text-[13.5px] font-semibold text-primary hover:bg-elevated focus-visible:outline-2 focus-visible:outline-primary"
        @click="startNewFolder"
      >
        <span class="flex size-6.5 shrink-0 items-center justify-center rounded-lg border-[1.5px] border-dashed border-primary">
          <UIcon name="i-lucide-plus" class="size-3.5" />
        </span>
        {{ t('tools.cleanup.newFolder') }}
      </button>
    </template>
  </USelectMenu>

  <UModal v-model:open="naming" :title="t('app.folderDialog.new')" :description="account ? accountName(account) : undefined" :ui="{ content: 'max-w-md' }">
    <template #body>
      <form class="flex gap-2" @submit.prevent="createFolder">
        <UInput v-model="name" :placeholder="t('app.folderDialog.placeholder')" :aria-label="t('app.folderDialog.placeholder')" autofocus size="lg" class="flex-1" />
        <UButton type="submit" :label="t('app.folderDialog.create')" size="lg" :loading="creating" :disabled="!name.trim()" />
      </form>
    </template>
  </UModal>
</template>
