<script setup lang="ts">
const props = defineProps<{ query: string }>()
const { add, list } = useSavedSearches()
const toast = useToast()
const { t } = useI18n()

/** Operators in the interface's language (the engine understands both). */
const operators = computed(() =>
  ['from', 'to', 'with', 'subject', 'attachment', 'read', 'pinned', 'labels', 'after', 'before', 'in']
    .map(k => [t(`mail.searchHelp.ops.${k}.op`), t(`mail.searchHelp.ops.${k}.desc`)] as const)
)

const name = ref('')
const saved = computed(() => list.value.some(s => s.query === props.query.trim()))
async function save() {
  await add(name.value, props.query.trim())
  name.value = ''
  toast.add({ title: t('mail.searchHelp.savedToast'), description: t('mail.searchHelp.savedToastDesc'), icon: 'i-lucide-bookmark' })
}
</script>

<template>
  <UPopover :content="{ align: 'end' }">
    <UButton
      :icon="query.trim() ? (saved ? 'i-lucide-bookmark-check' : 'i-lucide-bookmark-plus') : 'i-lucide-circle-help'"
      size="xs"
      color="neutral"
      variant="link"
      :aria-label="query.trim() ? $t('mail.searchHelp.saveSearch') : $t('mail.searchHelp.help')"
    />
    <template #content>
      <div class="w-80 space-y-3 p-3 text-sm">
        <form v-if="query.trim()" class="space-y-2" @submit.prevent="save">
          <p class="font-medium text-highlighted">{{ saved ? $t('mail.searchHelp.alreadySaved') : $t('mail.searchHelp.saveThis') }}</p>
          <div class="flex gap-2">
            <UInput v-model="name" :placeholder="query.trim()" size="sm" class="flex-1" :aria-label="$t('mail.searchHelp.name')" />
            <UButton type="submit" :label="$t('mail.searchHelp.save')" size="sm" />
          </div>
        </form>
        <div>
          <p class="mb-1.5 font-medium text-highlighted">{{ $t('mail.searchHelp.operators') }}</p>
          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
            <template v-for="[op, what] in operators" :key="op">
              <dt class="font-mono text-primary">{{ op }}</dt>
              <dd class="text-muted">{{ what }}</dd>
            </template>
          </dl>
          <p class="mt-2 text-xs text-dimmed">{{ $t('mail.searchHelp.footer') }}</p>
        </div>
      </div>
    </template>
  </UPopover>
</template>
