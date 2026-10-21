<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import type { MailTemplate } from '~/composables/useTemplates'

const { templates, put, remove: drop } = useTemplates()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()
const back = useScreenBack('/settings')

const editing = ref<MailTemplate | null>(null)
const saving = ref(false)

const blank = (): MailTemplate => ({ id: crypto.randomUUID(), name: '', html: '<p></p>', quick: false })
const edit = (tpl: MailTemplate) => {
  editing.value = { ...tpl }
}

function excerpt(html: string) {
  const text = new DOMParser().parseFromString(html, 'text/html').body.textContent ?? ''
  return text.replace(/\s+/g, ' ').trim()
}
const empty = computed(() => !editing.value || !excerpt(editing.value.html))

async function save() {
  const tpl = editing.value
  if (!tpl || empty.value) return
  saving.value = true
  try {
    await put({ ...tpl, name: tpl.name.trim() })
    editing.value = null
  } catch (e) {
    showError(e)
  } finally {
    saving.value = false
  }
}

async function remove(tpl: MailTemplate) {
  const ok = await confirm(t('settings.templates.removeConfirm', { name: tpl.name || t('app.composer.templates.untitled') }), {
    title: t('settings.templates.remove'),
    kind: 'warning'
  })
  if (!ok) return
  try {
    await drop(tpl.id)
    toast.add({ title: t('settings.templates.removed'), icon: 'i-lucide-trash-2' })
  } catch (e) {
    showError(e)
  }
}

async function toggleQuick(tpl: MailTemplate, quick: boolean) {
  try {
    await put({ ...tpl, quick })
  } catch (e) {
    showError(e)
  }
}
</script>

<template>
  <UDashboardPanel id="templates">
    <template #header>
      <UDashboardNavbar :title="$t('settings.templates.title')">
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
          <UButton :label="$t('settings.templates.new')" icon="i-lucide-plus" @click="edit(blank())" />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="mx-auto flex w-full max-w-3xl flex-col gap-6 py-2">
        <p class="text-sm text-muted">{{ $t('settings.templates.intro') }}</p>

        <div v-if="!templates.length" class="flex flex-col items-center gap-3 rounded-lg border border-dashed border-default py-12 text-sm text-muted">
          <UIcon name="i-lucide-file-text" class="size-8" />
          {{ $t('settings.templates.empty') }}
          <UButton :label="$t('settings.templates.create')" icon="i-lucide-plus" variant="outline" color="neutral" @click="edit(blank())" />
        </div>

        <div v-else class="divide-y divide-default overflow-hidden rounded-lg border border-default">
          <div v-for="tpl in templates" :key="tpl.id" class="flex items-center gap-3 px-4 py-3">
            <div class="min-w-0 flex-1">
              <p class="truncate font-medium text-highlighted">{{ tpl.name || $t('app.composer.templates.untitled') }}</p>
              <p class="truncate text-xs text-muted">{{ excerpt(tpl.html) }}</p>
            </div>
            <UTooltip :text="$t('settings.templates.quickHelp')">
              <label class="flex shrink-0 items-center gap-2 text-xs text-muted">
                <USwitch :model-value="tpl.quick" size="sm" @update:model-value="(v: boolean) => toggleQuick(tpl, v)" />
                <span class="max-sm:sr-only">{{ $t('settings.templates.quick') }}</span>
              </label>
            </UTooltip>
            <div class="flex shrink-0 items-center">
              <UTooltip :text="$t('settings.templates.edit')"><UButton icon="i-lucide-pencil" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.templates.edit')" @click="edit(tpl)" /></UTooltip>
              <UTooltip :text="$t('settings.templates.remove')"><UButton icon="i-lucide-trash-2" size="xs" color="neutral" variant="ghost" :aria-label="$t('settings.templates.remove')" @click="remove(tpl)" /></UTooltip>
            </div>
          </div>
        </div>
      </div>

      <USlideover
        :open="editing !== null"
        :title="templates.some(x => x.id === editing?.id) ? $t('settings.templates.editTitle') : $t('settings.templates.new')"
        :ui="{ content: 'max-w-2xl', body: 'flex flex-col gap-5' }"
        @update:open="(o) => { if (!o) editing = null }"
      >
        <template #body>
          <template v-if="editing">
            <UFormField :label="$t('settings.templates.name')">
              <UInput v-model="editing.name" :placeholder="$t('settings.templates.namePlaceholder')" class="w-full" />
            </UFormField>
            <UFormField :label="$t('settings.templates.text')" class="flex min-h-72 flex-1 flex-col" :ui="{ container: 'flex flex-1 flex-col' }">
              <MailEditor v-model="editing.html" :placeholder="$t('settings.templates.textPlaceholder')" />
            </UFormField>
            <UCheckbox v-model="editing.quick" :label="$t('settings.templates.quickLabel')" :description="$t('settings.templates.quickHelp')" />
          </template>
        </template>
        <template #footer>
          <div class="flex w-full items-center justify-end gap-2">
            <UButton :label="$t('settings.templates.cancel')" color="neutral" variant="ghost" @click="editing = null" />
            <UButton :label="$t('settings.templates.save')" :disabled="empty" :loading="saving" @click="save" />
          </div>
        </template>
      </USlideover>
    </template>
  </UDashboardPanel>
</template>
