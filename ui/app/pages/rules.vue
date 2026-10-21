<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import type { Rule, RuleAction, RuleCondition, RuleField, RuleOp, RulePreview } from '~/utils/types'

const route = useRoute()
const router = useRouter()
const { accounts, mailboxes } = useMail()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()
const back = useScreenBack('/settings')

const rules = ref<Rule[]>([])
async function load() {
  try {
    rules.value = await api.rules()
  } catch (e) {
    showError(e)
  }
}

const fields: RuleField[] = ['from', 'to', 'subject', 'list_id', 'has_attachment', 'bulk']
const ops: RuleOp[] = ['contains', 'not_contains', 'is', 'ends_with']
const fieldLabel = (f: RuleField) => t(`settings.rules.field.${f}`)
const opLabel = (o: RuleOp) => t(`settings.rules.op.${o}`)
const fieldItems = computed(() => fields.map(f => ({ label: fieldLabel(f), value: f })))
const opItems = computed(() => ops.map(o => ({ label: opLabel(o), value: o })))
/** Fields without a value: yes or no only (stored as "is" / "not_contains"). */
const flagField = (f: RuleField) => f === 'has_attachment' || f === 'bulk'
const yesNoItems = computed(() => [{ label: t('settings.rules.yes'), value: 'is' }, { label: t('settings.rules.no'), value: 'not_contains' }])

const folderName = (id: number) => {
  const m = mailboxes.value.find(b => b.id === id)
  return m ? (m.role ? roleLabels[m.role] : m.display_name) : t('settings.rules.deletedFolder')
}

function describeCondition(c: RuleCondition) {
  const no = c.op === 'not_contains'
  if (c.field === 'has_attachment') return no ? t('settings.rules.noAttachment') : t('settings.rules.hasAttachment')
  if (c.field === 'bulk') return no ? t('settings.rules.notBulk') : t('settings.rules.isBulk')
  return t('settings.rules.condition', { field: fieldLabel(c.field).toLowerCase(), op: opLabel(c.op), value: c.value })
}
function describeAction(a: RuleAction) {
  switch (a.kind) {
    case 'move': return t('settings.rules.action.move', { folder: folderName(a.mailbox_id) })
    default: return t(`settings.rules.action.${a.kind}`)
  }
}
/** "conditions: actions" without the leading "If"; also the default rule name. */
const ruleBody = (r: Rule) => t('settings.rules.body', {
  conditions: r.conditions.map(describeCondition).join(` ${r.match_all ? t('settings.rules.and') : t('settings.rules.or')} `),
  actions: r.actions.map(describeAction).join(', ')
})
const summary = (r: Rule) => t('settings.rules.summary', { body: ruleBody(r) })

const byAccount = computed(() => accounts.value
  .map(a => ({ account: a, rules: rules.value.filter(r => r.account_id === a.id) }))
  .filter(g => g.rules.length))

type Destination = 'none' | 'move' | 'archive' | 'trash'
interface Editing {
  rule: Rule
  destination: Destination
  folder: number | undefined
  markRead: boolean
  pin: boolean
}
const editing = ref<Editing | null>(null)

function edit(rule: Rule) {
  const move = rule.actions.find(a => a.kind === 'move')
  const destination = (rule.actions.find(a => a.kind === 'move' || a.kind === 'archive' || a.kind === 'trash')?.kind ?? 'none') as Destination
  editing.value = {
    rule: structuredClone(toRaw(rule)),
    destination,
    folder: move?.kind === 'move' ? move.mailbox_id : undefined,
    markRead: rule.actions.some(a => a.kind === 'mark_read'),
    pin: rule.actions.some(a => a.kind === 'pin')
  }
}

function blank(prefill?: { field: RuleField, value: string }, account = accounts.value[0]?.id ?? 0): Rule {
  return {
    id: 0,
    account_id: account,
    name: '',
    enabled: true,
    match_all: true,
    conditions: [prefill ? { field: prefill.field, op: prefill.field === 'from' ? 'contains' : 'is', value: prefill.value } : { field: 'from', op: 'contains', value: '' }],
    actions: [],
    stop: false
  }
}

const folderItems = computed(() => mailboxes.value
  .filter(m => m.account_id === editing.value?.rule.account_id && m.selectable && m.role !== 'inbox')
  .map(m => ({ label: m.role ? roleLabels[m.role] : m.display_name, value: m.id, icon: m.role ? roleIcons[m.role] : 'i-lucide-folder' })))

watch(() => editing.value?.rule.account_id, (next, prev) => {
  if (editing.value && prev !== undefined && next !== prev) editing.value.folder = undefined
})

const actionsOf = (e: Editing): RuleAction[] => [
  ...(e.markRead ? [{ kind: 'mark_read' } as const] : []),
  ...(e.pin ? [{ kind: 'pin' } as const] : []),
  ...(e.destination === 'move' && e.folder !== undefined ? [{ kind: 'move', mailbox_id: e.folder } as const] : []),
  ...(e.destination === 'archive' ? [{ kind: 'archive' } as const] : []),
  ...(e.destination === 'trash' ? [{ kind: 'trash' } as const] : [])
]
const problem = computed(() => {
  const e = editing.value
  if (!e) return null
  if (!e.rule.conditions.some(c => flagField(c.field) || c.value.trim())) return t('settings.rules.needCondition')
  if (e.destination === 'move' && e.folder === undefined) return t('settings.rules.needFolder')
  if (!actionsOf(e).length) return t('settings.rules.needAction')
  return null
})

const saving = ref(false)
async function save(run = false) {
  const e = editing.value
  if (!e || problem.value) return
  saving.value = true
  try {
    const rule: Rule = {
      ...e.rule,
      name: e.rule.name.trim() || `${ruleBody({ ...e.rule, actions: actionsOf(e) })}.`.slice(0, 57),
      actions: actionsOf(e),
      conditions: e.rule.conditions.map(c => (flagField(c.field) ? { ...c, value: '' } : { ...c, value: c.value.trim() }))
    }
    if (run && rule.enabled) {
      // Preview first: the rule is saved and applied only once confirmed.
      openPreview(rule, async () => {
        const id = await api.saveRule(rule)
        editing.value = null
        await load()
        await apply(id)
      })
      return
    }
    await api.saveRule(rule)
    editing.value = null
    await load()
  } catch (err) {
    showError(err, t('settings.rules.saveFailed'))
  } finally {
    saving.value = false
  }
}

async function apply(id: number) {
  const n = await api.runRule(id)
  toast.add({ title: n ? t('settings.rules.processed', { n }, n) : t('settings.rules.nothingProcessed'), icon: 'i-lucide-play' })
}

function runNow(id: number) {
  const rule = rules.value.find(r => r.id === id)
  // Applying skips disabled rules: the preview would announce messages left untouched.
  if (rule?.enabled) openPreview(rule, () => apply(id))
}

const preview = ref<{ rule: Rule, data: RulePreview | null, confirm: () => Promise<void> } | null>(null)
const applying = ref(false)

async function openPreview(rule: Rule, confirm: () => Promise<void>) {
  preview.value = { rule, data: null, confirm }
  try {
    const data = await api.previewRule(rule)
    if (preview.value?.rule === rule) preview.value = { ...preview.value, data }
  } catch (e) {
    preview.value = null
    showError(e)
  }
}

async function confirmPreview() {
  const p = preview.value
  if (!p || applying.value) return
  applying.value = true
  try {
    await p.confirm()
    preview.value = null
  } catch (e) {
    showError(e)
  } finally {
    applying.value = false
  }
}

async function toggle(rule: Rule, enabled: boolean) {
  try {
    await api.saveRule({ ...rule, enabled })
    rule.enabled = enabled
  } catch (e) {
    showError(e)
  }
}

async function remove(rule: Rule) {
  if (!await confirm(t('settings.rules.removeConfirm', { name: rule.name }), { title: t('settings.rules.removeTitle'), kind: 'warning' })) return
  try {
    await api.deleteRule(rule.id)
    await load()
  } catch (e) {
    showError(e)
  }
}

async function shift(rule: Rule, step: number) {
  const list = rules.value.filter(r => r.account_id === rule.account_id)
  const i = list.findIndex(r => r.id === rule.id)
  const j = i + step
  if (j < 0 || j >= list.length) return
  ;[list[i], list[j]] = [list[j]!, list[i]!]
  const others = rules.value.filter(r => r.account_id !== rule.account_id)
  try {
    await api.reorderRules([...others, ...list].map(r => r.id))
    await load()
  } catch (e) {
    showError(e)
  }
}

onMounted(async () => {
  await load()
  // From "Create a rule for this sender": open the editor prefilled.
  const from = route.query.from
  if (typeof from === 'string' && from) {
    edit(blank({ field: 'from', value: from }, Number(route.query.account) || undefined))
    router.replace({ query: {} })
  }
})
</script>

<template>
  <UDashboardPanel id="rules">
    <template #header>
      <UDashboardNavbar :title="$t('settings.rules.title')">
        <template #leading>
          <!-- Below lg the sidebar is a drawer: offer a way back instead. -->
          <UButton
            icon="i-lucide-arrow-left"
            color="neutral"
            variant="ghost"
            class="size-11 justify-center lg:hidden"
            :aria-label="$t('tools.shared.back')"
            @click="back"
          />
        </template>
        <template #right>
          <UButton :label="$t('settings.rules.new')" icon="i-lucide-plus" :disabled="!accounts.length" @click="edit(blank())" />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="mx-auto flex w-full max-w-3xl flex-col gap-6 py-2">
        <p class="text-sm text-muted">
          {{ $t('settings.rules.intro') }}
        </p>

        <div v-if="!rules.length" class="flex flex-col items-center gap-3 rounded-lg border border-dashed border-default py-12 text-sm text-muted">
          <UIcon name="i-lucide-workflow" class="size-8" />
          {{ $t('settings.rules.empty') }}
          <UButton :label="$t('settings.rules.create')" icon="i-lucide-plus" variant="outline" color="neutral" :disabled="!accounts.length" @click="edit(blank())" />
        </div>

        <section v-for="group in byAccount" :key="group.account.id">
          <h2 v-if="accounts.length > 1" class="mb-2 text-sm font-medium text-muted">{{ accountName(group.account) }}</h2>
          <div class="divide-y divide-default overflow-hidden rounded-lg border border-default">
            <div v-for="(r, i) in group.rules" :key="r.id" class="flex items-start gap-3 px-4 py-3" :class="r.enabled ? '' : 'opacity-60'">
              <USwitch :model-value="r.enabled" class="mt-0.5" :aria-label="r.enabled ? $t('settings.rules.disable') : $t('settings.rules.enable')" @update:model-value="(v: boolean) => toggle(r, v)" />
              <div class="min-w-0 flex-1">
                <p class="font-medium text-highlighted">{{ r.name }}</p>
                <p class="text-xs text-muted">{{ summary(r) }}<template v-if="r.stop"> {{ $t('settings.rules.stopNote') }}</template></p>
              </div>
              <div class="flex shrink-0 items-center">
                <UTooltip :text="$t('settings.rules.up')"><UButton icon="i-lucide-chevron-up" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.rules.up')" :disabled="i === 0" @click="shift(r, -1)" /></UTooltip>
                <UTooltip :text="$t('settings.rules.down')"><UButton icon="i-lucide-chevron-down" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.rules.down')" :disabled="i === group.rules.length - 1" @click="shift(r, 1)" /></UTooltip>
                <UTooltip :text="r.enabled ? $t('settings.rules.runTooltip') : $t('settings.rules.runDisabled')">
                  <span class="inline-flex"><UButton icon="i-lucide-play" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.rules.run')" :disabled="!r.enabled" @click="runNow(r.id)" /></span>
                </UTooltip>
                <UTooltip :text="$t('settings.rules.edit')"><UButton icon="i-lucide-pencil" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.rules.edit')" @click="edit(r)" /></UTooltip>
                <UTooltip :text="$t('settings.rules.remove')"><UButton icon="i-lucide-trash-2" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.rules.remove')" @click="remove(r)" /></UTooltip>
              </div>
            </div>
          </div>
        </section>
      </div>

      <USlideover
        :open="editing !== null"
        :title="editing?.rule.id ? $t('settings.rules.editTitle') : $t('settings.rules.new')"
        :ui="{ content: 'max-w-xl', body: 'space-y-5' }"
        @update:open="(o) => { if (!o) editing = null }"
      >
        <template #body>
          <template v-if="editing">
            <UFormField :label="$t('settings.rules.name')" :hint="$t('settings.rules.optional')">
              <UInput v-model="editing.rule.name" :placeholder="$t('settings.rules.namePlaceholder')" class="w-full" />
            </UFormField>
            <UFormField v-if="accounts.length > 1" :label="$t('settings.rules.account')">
              <USelect v-model="editing.rule.account_id" :items="accounts.map(a => ({ label: accountName(a), value: a.id }))" class="w-full" />
            </UFormField>

            <section class="space-y-2">
              <div class="flex flex-wrap items-center gap-2 text-sm">
                <span class="font-medium text-highlighted">{{ $t('settings.rules.when') }}</span>
                <USelect
                  v-model="editing.rule.match_all"
                  :items="[{ label: $t('settings.rules.matchAll'), value: true }, { label: $t('settings.rules.matchAny'), value: false }]"
                  :aria-label="$t('settings.rules.matchLabel')"
                  size="sm"
                  class="w-52"
                />
                <span class="text-muted">{{ $t('settings.rules.match') }}</span>
              </div>
              <div v-for="(c, i) in editing.rule.conditions" :key="i" class="flex flex-wrap items-center gap-2">
                <USelect v-model="c.field" :items="fieldItems" :aria-label="$t('settings.rules.fieldLabel')" size="sm" class="w-48" />
                <USelect v-if="flagField(c.field)" v-model="c.op" :items="yesNoItems" :aria-label="$t('settings.rules.opLabel')" size="sm" class="w-24" />
                <template v-else>
                  <USelect v-model="c.op" :items="opItems" :aria-label="$t('settings.rules.opLabel')" size="sm" class="w-40" />
                  <UInput v-model="c.value" :aria-label="$t('settings.rules.valueLabel')" size="sm" class="min-w-40 flex-1" :placeholder="c.field === 'from' ? $t('settings.rules.valuePlaceholder') : ''" />
                </template>
                <UButton
                  icon="i-lucide-x"
                  size="xs"
                  color="neutral"
                  variant="ghost"
                  :aria-label="$t('settings.rules.removeCondition')"
                  :disabled="editing.rule.conditions.length === 1"
                  @click="editing.rule.conditions.splice(i, 1)"
                />
              </div>
              <UButton
                :label="$t('settings.rules.addCondition')"
                icon="i-lucide-plus"
                size="xs"
                color="neutral"
                variant="ghost"
                @click="editing.rule.conditions.push({ field: 'subject', op: 'contains', value: '' })"
              />
            </section>

            <section class="space-y-3">
              <p class="text-sm font-medium text-highlighted">{{ $t('settings.rules.then') }}</p>
              <UTabs
                v-model="editing.destination"
                :items="[
                  { label: $t('settings.rules.keep'), value: 'none', icon: 'i-lucide-inbox' },
                  { label: $t('settings.rules.moveTo'), value: 'move', icon: 'i-lucide-folder-input' },
                  { label: $t('settings.rules.archive'), value: 'archive', icon: 'i-lucide-archive' },
                  { label: $t('settings.rules.trash'), value: 'trash', icon: 'i-lucide-trash-2' }
                ]"
                :content="false"
                size="sm"
                variant="pill"
                class="w-full"
                :ui="{ list: 'w-full', trigger: 'flex-1 justify-center', label: 'truncate' }"
              />
              <USelect
                v-if="editing.destination === 'move'"
                v-model="editing.folder"
                :items="folderItems"
                :placeholder="$t('settings.rules.folder')"
                class="w-full"
              />
              <div class="flex flex-col gap-2">
                <UCheckbox v-model="editing.markRead" :label="$t('settings.rules.markRead')" />
                <UCheckbox v-model="editing.pin" :label="$t('settings.rules.pin')" />
                <UCheckbox v-model="editing.rule.stop" :label="$t('settings.rules.stop')" />
              </div>
            </section>

            <p v-if="problem" class="text-xs text-warning">{{ problem }}</p>
            <p v-else-if="!editing.rule.enabled" class="text-xs text-muted">{{ $t('settings.rules.runDisabled') }}</p>
          </template>
        </template>
        <template #footer>
          <div class="flex w-full flex-wrap items-center justify-end gap-2">
            <UButton :label="$t('settings.rules.cancel')" color="neutral" variant="ghost" @click="editing = null" />
            <UButton :label="$t('settings.rules.saveAndRun')" color="neutral" variant="outline" :disabled="!!problem || !editing?.rule.enabled" :loading="saving" @click="save(true)" />
            <UButton :label="$t('settings.rules.save')" :disabled="!!problem" :loading="saving" @click="save()" />
          </div>
        </template>
      </USlideover>

      <RulePreviewModal
        :open="preview !== null"
        :rule-name="preview?.rule.name || ''"
        :summary="preview ? summary(preview.rule) : ''"
        :data="preview?.data ?? null"
        :applying="applying"
        @close="preview = null"
        @apply="confirmPreview"
      />
    </template>
  </UDashboardPanel>
</template>
