<script setup lang="ts">
/** `touch` grows the hit area of a lone box to 44 px without moving it. `mixed`: only some are
 *  checked. */
const model = defineModel<boolean>({ required: true })
defineProps<{
  label?: string
  mixed?: boolean
  touch?: boolean
}>()
const slots = defineSlots<{ default?: () => unknown }>()
</script>

<template>
  <button
    type="button"
    role="checkbox"
    :aria-checked="mixed ? 'mixed' : model"
    :aria-label="label"
    class="group flex shrink-0 items-center gap-2.5 rounded-lg text-start focus-visible:outline-2 focus-visible:outline-primary"
    :class="touch && !slots.default ? '-m-2.75 size-11 justify-center' : ''"
    @click.stop="model = !model"
  >
    <span
      class="flex shrink-0 items-center justify-center border-[1.5px] transition-colors"
      :class="[
        touch ? 'size-5.5 rounded-[7px]' : 'size-5 rounded-md',
        model || mixed ? 'border-primary bg-primary text-inverted' : 'border-accented group-hover:border-dimmed'
      ]"
    >
      <UIcon v-if="mixed" name="i-lucide-minus" class="size-3.5" />
      <UIcon v-else-if="model" name="i-lucide-check" class="size-3.5" />
    </span>
    <slot />
  </button>
</template>
