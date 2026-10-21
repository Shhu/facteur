<script setup lang="ts">
import twColors from 'tailwindcss/colors'
import type { AccountColorName } from '~/utils/accountColors'
import type { Account } from '~/utils/types'

/** Clicking the selected colour again reverts to the automatic one. */
const props = withDefaults(defineProps<{
  account: Account
  size?: 'md' | 'lg'
}>(), { size: 'md' })
const { accounts, loadAccounts } = useMail()
const showError = useErrorToast()
const { t } = useI18n()

const colorName = (c: string) => t(`settings.appearance.colors.${c}`)
const current = computed(() => accountColorName(props.account, accounts.value))

/** CSS variables so the selection ring matches the swatch (600 light, 400 dark, like the
 *  account's `dot`). */
const palette = (twColors as unknown as Record<string, Record<number, string>>)
const vars = (c: AccountColorName) => ({ '--sw': palette[c]?.[600], '--sw-dark': palette[c]?.[400] })

async function choose(color: AccountColorName) {
  const next = props.account.color === color ? '' : color
  try {
    await api.setAccountColor(props.account.id, next)
    // The engine emits no event for this change.
    await loadAccounts()
  } catch (e) {
    showError(e)
  }
}
</script>

<template>
  <div class="flex flex-col gap-2.5">
    <div
      role="radiogroup"
      :aria-label="$t('settings.accounts.colorTitle')"
      :class="size === 'lg' ? 'grid grid-cols-5 justify-items-center gap-2.5' : 'flex flex-wrap gap-2'"
    >
      <UTooltip v-for="c in ACCOUNT_COLOR_NAMES" :key="c" :text="colorName(c)" :disabled="size === 'lg'">
        <button
          type="button"
          role="radio"
          class="flex items-center justify-center rounded-full bg-(--sw) text-white ring-offset-(--surface-card) transition-transform hover:scale-110 dark:bg-(--sw-dark) dark:text-(--ui-color-neutral-900)"
          :class="[
            size === 'lg' ? 'size-10' : 'size-6.5',
            current === c ? (size === 'lg' ? 'ring-2 ring-(--sw) ring-offset-3 dark:ring-(--sw-dark)' : 'ring-2 ring-(--sw) ring-offset-2 dark:ring-(--sw-dark)') : ''
          ]"
          :style="vars(c)"
          :aria-label="colorName(c)"
          :aria-checked="current === c"
          @click="choose(c)"
        >
          <UIcon v-if="size === 'lg' && current === c" name="i-lucide-check" class="size-5" />
        </button>
      </UTooltip>
    </div>
    <span v-if="!account.color" class="text-xs text-dimmed">{{ $t('settings.accounts.colorAutoHint') }}</span>
  </div>
</template>
