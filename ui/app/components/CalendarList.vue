<script setup lang="ts">
defineProps<{ touch?: boolean }>()
const { accounts, accountColor } = useMail()
const { calendarsOf, statusOf, isIgnored, calendarColor, setEnabled, sync, isSyncing, lastSync } = useCalendar()
const synced = useSyncedLabel()
const { t } = useI18n()

const groups = computed(() => accounts.value.filter(a => !isIgnored(a.id)).map(a => ({ account: a, calendars: calendarsOf(a.id), status: statusOf(a.id) })))
const syncedLabel = computed(() => synced(lastSync()))
</script>

<template>
  <div class="flex flex-col gap-4">
    <section v-for="g in groups" :key="g.account.id" class="flex flex-col gap-1" :aria-label="accountName(g.account)">
      <h3 class="flex items-center gap-2 px-1.5 text-xs font-bold uppercase tracking-wider text-muted">
        <span class="size-2 shrink-0 rounded-full" :class="accountColor(g.account.id).dot" />
        <span class="truncate">{{ accountName(g.account) }}</span>
      </h3>
      <button
        v-for="c in g.calendars"
        :key="c.id"
        type="button"
        role="checkbox"
        :aria-checked="c.enabled"
        class="group flex w-full items-center gap-2.5 rounded-lg px-1.5 text-start transition-colors hover:bg-elevated/60 focus-visible:outline-2 focus-visible:outline-primary"
        :class="touch ? 'min-h-12 text-[15px]' : 'min-h-8 text-[13px]'"
        @click="setEnabled(c, !c.enabled)"
      >
        <span
          class="flex shrink-0 items-center justify-center rounded-[5px] border-2 transition-colors"
          :class="touch ? 'size-5' : 'size-4'"
          :style="{ borderColor: calendarColor(c.id), background: c.enabled ? calendarColor(c.id) : 'transparent' }"
        >
          <UIcon v-if="c.enabled" name="i-lucide-check" class="size-3 text-white" />
        </span>
        <span class="min-w-0 flex-1 truncate" :class="c.enabled ? 'text-highlighted' : 'text-muted'">{{ c.name }}</span>
        <UTooltip v-if="c.error" :text="c.error">
          <UIcon name="i-lucide-triangle-alert" class="size-3.25 shrink-0 text-warning" :aria-label="c.error" />
        </UTooltip>
        <UTooltip v-if="c.read_only" :text="t('tools.calendar.readOnly')">
          <UIcon name="i-lucide-lock" class="size-3.25 shrink-0 text-dimmed" :aria-label="t('tools.calendar.readOnly')" />
        </UTooltip>
      </button>
      <CalendarAccountState :account="g.account" :status="g.status" :touch="touch" class="mx-1.5 mt-0.5" />
      <p v-if="!g.calendars.length && (!g.status || g.status.state === 'ok')" class="px-1.5 text-[12.5px] text-dimmed">
        {{ t('tools.calendar.noCalendar') }}
      </p>
    </section>

    <div class="flex items-center gap-1 border-t border-default pt-3">
      <span class="min-w-0 flex-1 truncate px-1.5 text-xs text-muted">{{ isSyncing() ? t('common.syncing') : syncedLabel }}</span>
      <UTooltip :text="t('tools.calendar.refresh')">
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="ghost"
          :size="touch ? 'lg' : 'sm'"
          :class="touch ? 'size-11 justify-center' : ''"
          :loading="isSyncing()"
          :aria-label="t('tools.calendar.refresh')"
          @click="sync(null)"
        />
      </UTooltip>
    </div>
  </div>
</template>
