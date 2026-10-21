export interface SavedSearch {
  name: string
  query: string
}

export function useSavedSearches() {
  const list = useState<SavedSearch[]>('saved-searches', () => [])
  const showError = useErrorToast()

  async function load() {
    try {
      list.value = JSON.parse(await api.getSetting('saved_searches') ?? '[]')
    } catch {
      list.value = []
    }
  }

  async function persist(next: SavedSearch[]) {
    const before = list.value
    list.value = next
    try {
      await api.setSetting('saved_searches', JSON.stringify(next))
    } catch (e) {
      list.value = before
      showError(e, tr('common.toast.searchNotSaved'))
    }
  }

  const add = (name: string, query: string) =>
    persist([...list.value.filter(s => s.query !== query), { name: name.trim() || query, query }])
  const remove = (query: string) => persist(list.value.filter(s => s.query !== query))

  return { list, load, add, remove }
}
