<script setup lang="ts">
const props = defineProps<{ accountId: number }>()
const { accountById, accountColor } = useMail()
const account = computed(() => accountById(props.accountId))
const color = computed(() => accountColor(props.accountId))
</script>

<template>
  <span
    v-if="account"
    class="inline-flex max-w-44 flex-none items-center gap-1.5 rounded-full py-0.5 ps-[7px] pe-[9px] text-xs font-semibold"
    :class="color.soft"
  >
    <span class="size-[7px] flex-none rounded-full" :class="color.dot" />
    <span class="truncate">{{ accountName(account) }}</span>
  </span>
</template>
