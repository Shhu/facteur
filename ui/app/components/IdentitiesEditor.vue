<script setup lang="ts">
import type { Account, Identity } from '~/utils/types'

const props = withDefaults(defineProps<{
  account: Account
  touch?: boolean
}>(), { touch: false })
const { loadAccounts } = useMail()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()

const list = ref<Identity[]>([])
const opened = ref<number | null>(null)
/** The saved aliases the list was copied from (JSON): the edits are measured against them. */
const base = ref('[]')
function reset() {
  list.value = structuredClone(toRaw(props.account.identities))
  base.value = JSON.stringify(props.account.identities)
}
const dirty = computed(() => JSON.stringify(list.value) !== base.value)
// Every sync reloads the accounts (new objects, same content): unsaved edits are kept, the list
// is copied again only for another account or when nothing was changed.
watch(() => props.account.id, () => {
  reset()
  opened.value = null
}, { immediate: true })
watch(() => JSON.stringify(props.account.identities), (saved) => {
  if (saved !== base.value && !dirty.value) reset()
})

function add() {
  list.value.push({ email: '', name: '', signature: '' })
  opened.value = list.value.length - 1
}

const saving = ref(false)
async function save() {
  saving.value = true
  try {
    await api.setIdentities(props.account.id, list.value.map(i => ({ ...i, signature: signatureContent(i.signature) ? i.signature : '' })))
    await loadAccounts()
    // Once the new account has reached the props: the saved aliases become the base.
    await nextTick()
    reset()
    toast.add({ title: t('app.identities.saved'), description: props.account.email, icon: 'i-lucide-check' })
  } catch (e) {
    showError(e, t('app.identities.notSaved'))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="flex flex-col gap-3">
    <div class="flex flex-wrap items-center gap-3">
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="text-sm font-semibold text-highlighted">{{ $t('app.identities.label') }}</span>
        <span class="text-[13px] text-muted">{{ $t('app.identities.hint') }}</span>
      </div>
      <UButton
        :label="$t('app.identities.add')"
        icon="i-lucide-plus"
        color="neutral"
        variant="outline"
        :size="touch ? 'lg' : 'sm'"
        class="border border-dashed border-accented ring-0"
        :class="touch ? 'min-h-11 w-full justify-center rounded-xl' : 'rounded-[9px]'"
        @click="add"
      />
    </div>
    <div v-for="(identity, i) in list" :key="i" class="rounded-xl border border-default p-2.5">
      <div class="flex flex-wrap items-center gap-2">
        <UInput
          v-model="identity.email"
          :placeholder="$t('app.identities.emailPlaceholder')"
          :size="touch ? 'lg' : 'sm'"
          class="min-w-48 flex-1"
          :aria-label="$t('app.identities.address')"
        />
        <UInput
          v-model="identity.name"
          :placeholder="account.name || $t('app.identities.displayName')"
          :size="touch ? 'lg' : 'sm'"
          :class="touch ? 'w-full' : 'w-44'"
          :aria-label="$t('app.identities.displayName')"
        />
        <UTooltip :text="$t('app.identities.signatureTooltip')">
          <UButton
            icon="i-lucide-signature"
            :size="touch ? 'lg' : 'sm'"
            :color="identity.signature ? 'primary' : 'neutral'"
            variant="ghost"
            :class="touch ? 'size-11 justify-center' : ''"
            :aria-label="$t('app.identities.signature')"
            @click="opened = opened === i ? null : i"
          />
        </UTooltip>
        <UButton
          icon="i-lucide-x"
          :size="touch ? 'lg' : 'sm'"
          color="neutral"
          variant="ghost"
          :class="touch ? 'size-11 justify-center' : ''"
          :aria-label="$t('app.identities.remove')"
          @click="list.splice(i, 1)"
        />
      </div>
      <div v-if="opened === i" class="mt-2">
        <MailEditor
          :model-value="signatureContent(identity.signature)"
          compact
          :touch="touch"
          :max-image-width="400"
          :placeholder="$t('app.identities.signaturePlaceholder')"
          @update:model-value="(v) => { identity.signature = v }"
        />
      </div>
    </div>
    <div v-if="dirty" class="flex">
      <UButton
        :label="$t('app.identities.save')"
        :size="touch ? 'lg' : 'sm'"
        :loading="saving"
        :class="touch ? 'min-h-11 w-full justify-center rounded-xl' : 'ms-auto'"
        @click="save"
      />
    </div>
    <p v-if="list.length" class="text-xs text-dimmed">
      {{ $t('app.identities.serverNote') }}
    </p>
  </div>
</template>
