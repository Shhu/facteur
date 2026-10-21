<script setup lang="ts">
const sidebarOpen = defineModel<boolean>('sidebarOpen', { required: true })
const { unifiedUnread } = useMail()
const { compose } = useComposer()
const paletteOpen = useState('command-palette', () => false)
const route = useRouter().currentRoute
const { t } = useI18n()
const { covered, close } = useMailOverlay()
const { calendarsAvailable } = useCalendar()

const tabs = computed(() => [
  { label: t('app.sidebar.home'), icon: 'i-lucide-house', active: route.value.path === '/' && !sidebarOpen.value, onClick: () => navigateTo('/') },
  {
    label: t('app.tabs.mailboxes'),
    icon: 'i-lucide-inbox',
    badge: unifiedUnread.value,
    active: sidebarOpen.value || route.value.path.startsWith('/mail'),
    // Over a list, the tab first returns to it as it was; on the list, it opens the drawer.
    onClick: () => {
      if (covered.value) close()
      else sidebarOpen.value = !sidebarOpen.value
    }
  },
  ...(calendarsAvailable.value
    ? [{ label: t('app.tabs.calendar'), icon: 'i-lucide-calendar', active: route.value.path === '/calendar' && !sidebarOpen.value, onClick: () => navigateTo('/calendar') }]
    : []),
  { label: t('app.tabs.search'), icon: 'i-lucide-search', active: paletteOpen.value, onClick: () => { paletteOpen.value = true } },
  { label: t('app.sidebar.settings'), icon: 'i-lucide-settings', active: route.value.path === '/settings' && !sidebarOpen.value, onClick: () => navigateTo('/settings') }
])
/** Not over an open message: the button would hide the text. */
const showFab = computed(() => (route.value.path === '/' || route.value.path.startsWith('/mail/')) && !route.value.query.thread)
</script>

<template>
  <UButton
    v-if="showFab"
    icon="i-lucide-pen-line"
    size="xl"
    square
    :aria-label="$t('app.sidebar.newMessage')"
    class="fixed end-4 bottom-[calc(5.5rem+env(safe-area-inset-bottom))] z-40 size-14.5 justify-center rounded-[20px] shadow-lg lg:hidden"
    :ui="{ leadingIcon: 'size-6' }"
    @click="compose()"
  />
  <nav
    :aria-label="$t('app.tabs.label')"
    class="fixed inset-x-0 bottom-0 z-40 grid h-[calc(4.5rem+env(safe-area-inset-bottom))] border-t border-default bg-default px-2 pt-2 pb-[calc(0.5rem+env(safe-area-inset-bottom))] lg:hidden"
    :class="tabs.length > 4 ? 'grid-cols-5' : 'grid-cols-4'"
  >
    <button
      v-for="tab in tabs"
      :key="tab.label"
      type="button"
      class="flex min-h-11 flex-col items-center justify-center gap-1 text-[11px]"
      :class="tab.active ? 'font-semibold text-primary' : 'font-medium text-muted'"
      :aria-current="tab.active ? 'page' : undefined"
      @click="tab.onClick"
    >
      <span class="relative flex h-7.5 w-14 items-center justify-center rounded-full" :class="tab.active ? 'bg-primary/15' : ''">
        <UIcon :name="tab.icon" class="size-5" />
        <span
          v-if="tab.badge"
          class="absolute top-0 right-2 h-4 min-w-4 rounded-full bg-primary px-1 text-center text-[10px] font-bold leading-4 text-inverted"
        >{{ tab.badge > 99 ? '99+' : tab.badge }}</span>
      </span>
      {{ tab.label }}
    </button>
  </nav>
</template>
