<script setup lang="ts">
import type { Account } from '~/utils/types'

const props = withDefaults(defineProps<{
  account: Account
  touch?: boolean
}>(), { touch: false })
const showError = useErrorToast()
const { t } = useI18n()

/** Saved a second after the last keystroke, without reloading the accounts: the editor would
 *  otherwise get a version behind the typing. */
let timer: ReturnType<typeof setTimeout> | undefined
/** Save waiting for the end of typing; run at once when the editor goes away (accordion
 *  collapsed, page left). */
let pending: (() => Promise<void>) | null = null
const saved = ref(false)
/** What the editor shows once edited: an accounts reload (end of a sync) while typing must not
 *  replace it with the saved version. */
const draft = ref<string | null>(null)

function edit(html: string) {
  const a = props.account
  draft.value = html
  saved.value = false
  const signature = signatureContent(html) ? html : ''
  clearTimeout(timer)
  pending = async () => {
    pending = null
    if (signature === a.signature) return
    try {
      await api.setSignature(a.id, signature)
      // The composer reads the signature from the shared account state.
      a.signature = signature
      saved.value = true
    } catch (e) {
      showError(e, t('settings.accounts.signatureFailed'))
    }
  }
  timer = setTimeout(() => pending?.(), 1000)
}
onBeforeUnmount(() => {
  clearTimeout(timer)
  pending?.()
})
</script>

<template>
  <div class="flex flex-col gap-2.5">
    <div class="flex flex-wrap items-baseline gap-x-2.5 gap-y-0.5" :class="touch ? 'flex-col' : ''">
      <span class="flex-1 font-semibold text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ $t('settings.accounts.signature') }}</span>
      <span class="text-[12.5px]" :class="saved ? 'text-success' : 'text-muted'">
        {{ saved ? $t('settings.accounts.signatureSaved') : $t('settings.accounts.signatureHint') }}
      </span>
    </div>
    <MailEditor
      :model-value="draft ?? signatureContent(account.signature)"
      compact
      :touch="touch"
      :max-image-width="400"
      :placeholder="$t('settings.accounts.signaturePlaceholder')"
      class="rounded-xl! bg-(--surface-card) [&_[role=toolbar]]:rounded-t-[11px] [&_[role=toolbar]]:bg-(--surface-side)"
      @update:model-value="edit"
    />
  </div>
</template>
