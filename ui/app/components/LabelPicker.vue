<script setup lang="ts">
import type { DropdownMenuItem } from '@nuxt/ui'

/** Gmail labels: half ticked when only some messages have one; toggling applies to all of them
 *  at once, and the messages stay in their folder. */
const props = withDefaults(defineProps<{ accountId: number, ids: number[], align?: 'start' | 'end' }>(), { align: 'start' })
const emit = defineEmits<{ applied: [token: number | null, title: string] }>()

const { mailboxes } = useMail()
const showError = useErrorToast()
const { t } = useI18n()

const open = defineModel<boolean>('open', { default: false })
/** The labels of each message, read when the menu opens and kept up to date by its changes. */
const held = ref(new Map<number, Set<number>>())

const labels = computed(() => mailboxes.value
  .filter(m => m.account_id === props.accountId && m.user_label)
  .sort((a, b) => a.display_name.localeCompare(b.display_name)))

watch(open, async (isOpen) => {
  if (!isOpen || !props.ids.length) return
  try {
    const found = await api.messageLabels(props.ids)
    held.value = new Map(props.ids.map(id => [id, new Set(found[id] ?? [])]))
  } catch (e) {
    showError(e)
  }
})

const count = (label: number) => props.ids.filter(id => held.value.get(id)?.has(label)).length

async function toggle(label: number, name: string) {
  const all = count(label) === props.ids.length
  try {
    const token = await api.setLabels(props.ids, all ? [] : [label], all ? [label] : [])
    for (const id of props.ids) {
      const set = held.value.get(id) ?? new Set<number>()
      if (all) set.delete(label)
      else set.add(label)
      held.value.set(id, set)
    }
    held.value = new Map(held.value)
    emit('applied', token, all ? t('mail.labels.removed', { label: name }) : t('mail.labels.added', { label: name }))
  } catch (e) {
    showError(e)
  }
}

const items = computed<DropdownMenuItem[][]>(() => [
  [{ type: 'label', label: t('mail.labels.title') }],
  labels.value.length
    ? labels.value.map((m) => {
        const n = count(m.id)
        return {
          label: m.display_name,
          icon: n === 0 ? 'i-lucide-square' : n === props.ids.length ? 'i-lucide-square-check' : 'i-lucide-square-minus',
          ui: { itemLeadingIcon: n ? 'text-primary' : 'text-dimmed' },
          onSelect: (e: Event) => {
            // The menu stays open: several labels in a row.
            e.preventDefault()
            toggle(m.id, m.display_name)
          }
        }
      })
    : [{ label: t('mail.labels.none'), disabled: true }]
])
</script>

<template>
  <UDropdownMenu v-model:open="open" :items="items" :content="{ align }" :ui="{ content: 'max-h-[60vh] min-w-52' }">
    <slot />
  </UDropdownMenu>
</template>
