export function useDarkMail() {
  const enabled = useDeviceSwitch('facteur:dark-mail')
  const colorMode = useColorMode()
  const active = computed(() => enabled.value && colorMode.value === 'dark')

  function set(value: boolean) {
    enabled.value = value
  }

  return { enabled, active, set }
}
