<script lang="ts">
import type { Mailbox } from '~/utils/types'

export interface FolderNode {
  mailbox: Mailbox
  label: string
  icon: string
  iconClass: string
  count?: string
  active: boolean
  activeClass?: string
  activeCountClass?: string
  children: FolderNode[]
}

/** Row styles shared by every entry of the sidebar (links, accounts, folders). No padding in
 *  `base`: each row sets its own, so that no two classes compete. */
export const sidebarRow = {
  base: 'flex w-full min-w-0 items-center gap-2.5 rounded-[10px] text-start text-sm transition-colors focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary',
  idle: 'text-default hover:bg-elevated hover:text-highlighted',
  active: 'bg-primary/14 font-semibold text-primary',
  activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-bold text-inverted'
}
</script>

<script setup lang="ts">
const props = defineProps<{ node: FolderNode, depth?: number }>()

const open = ref(true)
const hasChildren = computed(() => props.node.children.length > 0)
// Each level sits 1.5rem further in, after the row's own 0.625rem padding.
const indent = computed(() => ({ paddingInlineStart: `${0.625 + (props.depth ?? 0) * 1.5}rem` }))
</script>

<template>
  <div class="flex flex-col gap-px">
    <div class="relative flex items-center">
      <!-- Folder rows are links: dropping messages (folderUnder) and the context menu (pickTarget)
           find the folder from the <a> and its [data-mailbox-id] label. -->
      <NuxtLink
        v-if="node.mailbox.selectable"
        :to="`/mail/${node.mailbox.id}`"
        :class="[sidebarRow.base, 'h-8.5 pe-2.5', node.active ? node.activeClass ?? sidebarRow.active : sidebarRow.idle]"
        :style="indent"
      >
        <UIcon :name="node.icon" class="size-4 shrink-0" :class="node.active ? '' : node.iconClass" />
        <span class="flex-1 truncate" :data-mailbox-id="node.mailbox.id">{{ node.label }}</span>
        <span
          v-if="node.count"
          :class="node.active ? node.activeCountClass ?? sidebarRow.activeCount : 'shrink-0 text-xs font-semibold text-muted'"
        >{{ node.count }}</span>
        <span v-if="hasChildren" class="w-5 shrink-0" />
      </NuxtLink>
      <!-- Container that cannot be opened ([Gmail]…): the whole row folds its children. -->
      <button
        v-else
        type="button"
        :class="[sidebarRow.base, 'h-8.5 pe-2.5', sidebarRow.idle]"
        :style="indent"
        :aria-expanded="open"
        @click="open = !open"
      >
        <UIcon :name="node.icon" class="size-4 shrink-0" :class="node.iconClass" />
        <span class="flex-1 truncate" :data-mailbox-id="node.mailbox.id">{{ node.label }}</span>
        <UIcon name="i-lucide-chevron-down" class="size-3.5 shrink-0 text-dimmed transition-transform" :class="open ? 'rotate-180' : ''" />
      </button>
      <button
        v-if="node.mailbox.selectable && hasChildren"
        type="button"
        class="absolute end-1.5 flex size-6 items-center justify-center rounded-md text-dimmed transition-colors hover:bg-accented/60 hover:text-highlighted focus-visible:outline-2 focus-visible:outline-primary"
        :data-mailbox-id="node.mailbox.id"
        :aria-label="node.label"
        :aria-expanded="open"
        @click="open = !open"
      >
        <UIcon name="i-lucide-chevron-down" class="size-3.5 transition-transform" :class="open ? 'rotate-180' : ''" />
      </button>
    </div>
    <template v-if="open && hasChildren">
      <SidebarFolder v-for="child in node.children" :key="child.mailbox.id" :node="child" :depth="(depth ?? 0) + 1" />
    </template>
  </div>
</template>
