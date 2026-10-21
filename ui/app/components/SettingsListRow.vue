<script setup lang="ts">
import type { RouteLocationRaw } from 'vue-router'

const props = defineProps<{
  label: string
  sublabel?: string
  icon?: string
  tone?: string
  toneStyle?: Record<string, string>
  value?: string
  to?: RouteLocationRaw
  button?: boolean
  size?: 'md' | 'lg'
  /** Other words the settings search finds it by. */
  keywords?: string
}>()
const shown = useSettingMatch(() => [props.label, props.sublabel, props.value, props.keywords])
const emit = defineEmits<{ click: [] }>()

const NuxtLink = resolveComponent('NuxtLink')
const is = computed(() => (props.to ? NuxtLink : props.button ? 'button' : 'div'))
</script>

<template>
  <component
    :is="is"
    v-show="shown"
    :to="to"
    :type="button && !to ? 'button' : undefined"
    class="flex w-full items-center gap-3 py-2 text-start"
    :class="[size === 'lg' ? 'min-h-15 px-3.5' : 'min-h-13 px-3.5', to || button ? 'active:bg-elevated/60' : '']"
    @click="button ? emit('click') : undefined"
  >
    <slot name="leading">
      <span v-if="icon" class="flex size-7.5 shrink-0 items-center justify-center rounded-[9px] text-white" :class="tone" :style="toneStyle">
        <UIcon :name="icon" class="size-4.5" />
      </span>
    </slot>
    <span class="flex min-w-0 flex-1 flex-col gap-px">
      <span class="truncate text-[15px] text-highlighted">{{ label }}</span>
      <span v-if="sublabel" class="truncate text-[12.5px] text-muted">{{ sublabel }}</span>
    </span>
    <span v-if="value || $slots.value" class="inline-flex max-w-[45%] shrink-0 items-center gap-1.5 truncate text-sm text-muted">
      <slot name="value">{{ value }}</slot>
    </span>
    <slot>
      <UIcon v-if="to || button" name="i-lucide-chevron-right" class="size-4.5 shrink-0 text-dimmed" />
    </slot>
  </component>
</template>
