/** `quick`: also offered as a ready answer under the messages. Shared by every account. */
export interface MailTemplate {
  id: string
  name: string
  html: string
  quick: boolean
}

const KEY = 'templates'

function parse(raw: string | null): MailTemplate[] {
  if (!raw) return []
  try {
    const list = JSON.parse(raw)
    return Array.isArray(list)
      ? list.filter(t => t && typeof t.id === 'string' && typeof t.html === 'string').map(t => ({ id: t.id, name: String(t.name ?? ''), html: t.html, quick: !!t.quick }))
      : []
  } catch {
    return []
  }
}

export function useTemplates() {
  const templates = useState<MailTemplate[]>('mail-templates', () => [])
  const loaded = useState('mail-templates-loaded', () => false)
  const showError = useErrorToast()

  async function load() {
    try {
      templates.value = parse(await api.getSetting(KEY))
      loaded.value = true
    } catch (e) {
      showError(e)
    }
  }
  if (!loaded.value && import.meta.client) load()

  async function save(list: MailTemplate[]) {
    await api.setSetting(KEY, JSON.stringify(list))
    templates.value = list
  }

  function put(t: MailTemplate) {
    const list = templates.value.slice()
    const i = list.findIndex(x => x.id === t.id)
    if (i >= 0) list[i] = t
    else list.push(t)
    return save(list)
  }

  function remove(id: string) {
    return save(templates.value.filter(t => t.id !== id))
  }

  const quick = computed(() => templates.value.filter(t => t.quick))

  return { templates, quick, load, put, remove }
}
