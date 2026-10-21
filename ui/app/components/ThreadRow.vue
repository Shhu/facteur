<script setup lang="ts">
import type { ListItem } from '~/utils/types'
import type { ListAction } from '~/components/ThreadList.vue'

const props = defineProps<{
  item: ListItem
  opened: number | null
  selected: Set<string>
  selecting: boolean
  /** Key of the hovered row: only it carries the action buttons. */
  hovered: string | null
  actions: (item: ListItem) => { kind: ListAction, icon: string, label: string }[]
  /** DOM id of the row, the list's `aria-activedescendant` when it is the open thread. */
  rowId: string
}>()
const emit = defineEmits<{
  click: [e: MouseEvent, item: ListItem]
  dragstart: [e: DragEvent, item: ListItem]
  hover: [key: string | null]
  toggle: [item: ListItem, range: boolean]
  action: [kind: ListAction, item: ListItem]
}>()
const { t } = useI18n()

/** Invitation recognised from its subject (rows don't carry the ICS): a tag replaces the
 *  service's prefix, and Google's update preview is shortened. */
const invite = computed(() => inviteFromSubject(props.item.subject, props.item.attachments))
const inviteTag = computed(() => {
  switch (invite.value?.kind) {
    case 'update': return { label: t('mail.invite.tag.update'), tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300' }
    case 'cancelled': return { label: t('mail.invite.tag.cancelled'), tone: 'bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300' }
    case 'invitation': return { label: t('mail.invite.invitation'), tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300' }
    default: return null
  }
})
const subject = computed(() => invite.value?.title || props.item.subject || t('common.noSubject'))
const preview = computed(() => {
  const change = invite.value ? changeFromPreview(props.item.preview) : null
  return change ? t('mail.invite.changed', { what: change }) : props.item.preview
})
</script>

<template>
  <!-- `--list-bg` (set by the mail page) is the list background, used to cut the unread dot out of the avatar. -->
  <!-- An option of the list (ThreadList's listbox): selected when open, checked when ticked. -->
  <div
    :id="rowId"
    role="option"
    :aria-selected="opened === item.threadId"
    :aria-checked="selected.has(item.key)"
    draggable="true"
    class="group relative flex cursor-pointer items-start gap-3 rounded-xl px-2.5 py-3 max-lg:px-2"
    :class="[
      selected.has(item.key) ? 'bg-primary/15' : opened === item.threadId ? 'bg-primary/9' : 'hover:bg-elevated/60',
      opened === item.threadId && 'ring-1 ring-inset ring-primary/35'
    ]"
    @click="emit('click', $event, item)"
    @dragstart="emit('dragstart', $event, item)"
    @pointerenter="(e: PointerEvent) => e.pointerType === 'mouse' && emit('hover', item.key)"
    @pointerleave="emit('hover', null)"
  >
    <!-- Avatar, replaced by a checkbox on hover or in selection mode. Out of the tab order: the list
         takes the keys (Space ticks the open row, like `x`). -->
    <button
      type="button"
      role="checkbox"
      tabindex="-1"
      :aria-checked="selected.has(item.key)"
      :aria-label="$t('mail.threadRow.select')"
      class="relative block size-[38px] shrink-0 cursor-pointer max-lg:size-[42px]"
      @click.stop="emit('toggle', item, $event.shiftKey)"
    >
      <MailAvatar
        :address="item.avatar"
        :account-id="item.accountId"
        size="row"
        :class="selecting || selected.has(item.key) ? 'invisible' : 'group-hover:invisible'"
      />
      <span
        class="absolute inset-0 items-center justify-center"
        :class="selecting || selected.has(item.key) ? 'flex' : 'hidden group-hover:flex'"
      >
        <CheckMark :checked="selected.has(item.key)" />
      </span>
      <span
        v-if="item.unread"
        class="absolute -top-px -left-px size-2.5 rounded-full bg-primary ring-2 ring-(--list-bg) max-lg:size-[11px]"
      />
    </button>
    <span v-if="item.unread" class="sr-only">{{ $t('mail.threadRow.unread') }}</span>

    <div class="flex min-w-0 flex-1 flex-col gap-0.5">
      <!-- Fixed height, centred: the hover buttons (no text, so no baseline) must not change the
           row's height. -->
      <div class="flex h-5 items-center gap-1.5">
        <span class="truncate text-sm text-highlighted max-lg:text-[15px]" :class="item.unread ? 'font-bold' : 'font-medium'">
          {{ item.who }}
        </span>
        <span v-if="item.count > 1" class="h-[18px] shrink-0 rounded-full bg-elevated px-1.5 text-[11px] font-bold leading-[18px] text-muted">
          {{ item.count }}
        </span>

        <span v-if="hovered !== item.key" class="ms-auto flex shrink-0 items-center gap-1.5 ps-1 text-xs">
          <UIcon v-if="item.attachments" name="i-lucide-paperclip" class="size-3.5 text-muted" />
          <UIcon v-if="item.flagged" name="i-lucide-pin" class="size-3.5 text-primary" />
          <!-- Snoozed: the due time replaces the date. Back from snooze: an alarm icon says so. -->
          <template v-if="item.snoozedUntil">
            <UIcon name="i-lucide-alarm-clock" class="size-3.5 text-primary" />
            <span class="font-medium text-primary">{{ untilLabel(item.snoozedUntil) }}</span>
          </template>
          <template v-else>
            <UIcon v-if="item.woken" name="i-lucide-alarm-clock" class="size-3.5 text-primary" :title="$t('mail.threadRow.woken')" />
            <span :class="item.unread ? 'font-bold text-primary' : 'font-medium text-muted'">{{ shortDate(item.date) }}</span>
          </template>
        </span>
        <!-- Quick actions replace the date on hover: they push the text aside, and their height
             (20 px) is the row's, so it does not jump. -->
        <span v-else class="ms-auto flex h-5 shrink-0 items-center gap-0.5">
          <UTooltip v-for="a in actions(item)" :key="a.kind" :text="a.label">
            <UButton
              :icon="a.icon"
              size="xs"
              color="neutral"
              variant="ghost"
              :aria-label="a.label"
              class="size-5 justify-center p-0"
              :ui="{ leadingIcon: 'size-3.5' }"
              @click.stop="emit('action', a.kind, item)"
            />
          </UTooltip>
        </span>
      </div>
      <div class="flex items-baseline gap-2">
        <span
          v-if="inviteTag"
          class="inline-flex h-5 shrink-0 items-center gap-1 self-center rounded-md px-1.75 text-[11px] font-semibold"
          :class="inviteTag.tone"
        >
          <UIcon :name="invite?.kind === 'cancelled' ? 'i-lucide-calendar-x' : 'i-lucide-calendar'" class="size-3" />
          {{ inviteTag.label }}
        </span>
        <p
          class="min-w-0 flex-1 truncate text-[13.5px] max-lg:text-sm"
          :class="item.unread ? 'font-semibold text-highlighted' : 'text-default'"
        >
          {{ subject }}
        </p>
        <span v-if="item.labels?.length" class="flex max-w-[45%] shrink-0 items-center gap-1 self-center" :title="item.labels.join(', ')">
          <span
            v-for="l in item.labels.slice(0, 2)"
            :key="l"
            class="h-[18px] max-w-32 truncate rounded-md bg-elevated px-1.5 text-[11px] font-medium leading-[18px] text-toned"
          >{{ l }}</span>
          <span v-if="item.labels.length > 2" class="text-[11px] font-medium text-muted">+{{ item.labels.length - 2 }}</span>
        </span>
        <span v-if="item.accountLabel" class="flex max-w-[40%] shrink-0 items-center gap-1.5 text-xs text-muted" :title="item.accountLabel">
          <span class="size-2 shrink-0 rounded-full" :class="item.accountDot" />
          <span class="truncate">{{ item.accountLabel }}</span>
        </span>
      </div>
      <p class="truncate text-[12.5px] text-muted max-lg:text-[13px]">{{ item.preview || ' ' }}</p>
    </div>
  </div>
</template>
