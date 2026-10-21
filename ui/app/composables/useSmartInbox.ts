import type { Category } from '~/utils/types'

export function useSmartInbox() {
  const enabled = useDeviceSwitch('facteur:smart-inbox')
  const category = useState<Category | 'all'>('smart-category', () => 'people')

  function set(value: boolean) {
    enabled.value = value
  }

  return { enabled, category, set }
}
