<script setup lang="ts">
/** The search field of the calendar, with Google Calendar's filters (who, where, without,
 *  calendars, dates) in a popover. */
defineProps<{ touch?: boolean }>()
const emit = defineEmits<{ escape: [] }>()
const { form, open, filtered, resetFilters } = useCalendarSearch()
const { calendars } = useCalendar()
const { accounts } = useMail()
const { t } = useI18n()

const input = useTemplateRef<{ inputRef?: HTMLInputElement }>('input')
function focus() {
  open.value = true
  nextTick(() => input.value?.inputRef?.focus())
}
defineExpose({ focus })

const calendarItems = computed(() => {
  const several = new Set(calendars.value.filter(c => c.enabled).map(c => c.account_id)).size > 1
  return calendars.value
    .filter(c => c.enabled)
    .map(c => ({
      label: c.name,
      value: c.id,
      description: several ? accounts.value.find(a => a.id === c.account_id)?.email : undefined
    }))
})
const filtersOpen = ref(false)

function onEscape() {
  if (form.value.text) form.value.text = ''
  else emit('escape')
}
</script>

<template>
  <div class="flex min-w-0 items-center gap-1">
    <UInput
      ref="input"
      v-model="form.text"
      icon="i-lucide-search"
      :placeholder="t('tools.calendar.search.placeholder')"
      :aria-label="t('tools.calendar.search.label')"
      variant="soft"
      :size="touch ? 'lg' : 'md'"
      class="min-w-0 flex-1"
      :ui="{ base: touch ? 'min-h-11' : 'rounded-[10px]' }"
      @focus="open = true"
      @keydown.escape.prevent="onEscape"
    >
      <template v-if="!touch && !form.text" #trailing>
        <UKbd value="/" variant="outline" />
      </template>
    </UInput>
    <UPopover v-model:open="filtersOpen" :content="{ align: 'end', sideOffset: 6 }" :ui="{ content: 'w-80 max-w-[calc(100vw-2rem)] rounded-2xl p-4' }">
      <UChip :show="filtered" inset size="md">
        <UButton
          icon="i-lucide-sliders-horizontal"
          color="neutral"
          variant="ghost"
          :size="touch ? 'lg' : 'md'"
          :class="touch && 'size-11 justify-center rounded-xl'"
          :aria-label="t('tools.calendar.search.filters')"
        />
      </UChip>
      <template #content>
        <form class="flex flex-col gap-3" @submit.prevent="filtersOpen = false; open = true">
          <UFormField :label="t('tools.calendar.search.who')" :hint="t('tools.calendar.search.whoHint')" :ui="{ hint: 'text-xs' }">
            <UInput v-model="form.who" icon="i-lucide-users" class="w-full" />
          </UFormField>
          <UFormField :label="t('tools.calendar.search.where')">
            <UInput v-model="form.place" icon="i-lucide-map-pin" class="w-full" />
          </UFormField>
          <UFormField :label="t('tools.calendar.search.without')">
            <UInput v-model="form.without" icon="i-lucide-minus-circle" class="w-full" />
          </UFormField>
          <UFormField v-if="calendarItems.length > 1" :label="t('tools.calendar.search.calendars')">
            <USelectMenu
              v-model="form.calendars"
              :items="calendarItems"
              value-key="value"
              multiple
              :placeholder="t('tools.calendar.search.allCalendars')"
              :search-input="false"
              class="w-full"
            />
          </UFormField>
          <UFormField :label="t('tools.calendar.search.period')">
            <div class="flex items-center gap-2">
              <UInput v-model="form.from" type="date" :max="form.to || undefined" class="min-w-0 flex-1" :aria-label="t('tools.shared.from')" />
              <span class="text-sm text-muted">{{ t('tools.shared.between') }}</span>
              <UInput v-model="form.to" type="date" :min="form.from || undefined" class="min-w-0 flex-1" :aria-label="t('tools.shared.to')" />
            </div>
          </UFormField>
          <div class="flex justify-between gap-2 pt-1">
            <UButton :label="t('tools.calendar.search.reset')" color="neutral" variant="ghost" :disabled="!filtered" @click="resetFilters" />
            <UButton type="submit" :label="t('tools.calendar.search.apply')" />
          </div>
        </form>
      </template>
    </UPopover>
  </div>
</template>
