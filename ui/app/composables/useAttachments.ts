import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { confirm } from '~/composables/useConfirm'
import type { AttachmentReport } from '~/utils/types'

interface Viewed {
  messageId: number
  index: number
  filename: string
  mime: string
  url: string
  text: string | null
  /** PDF bytes, drawn by pdf.js, never by the system's built-in viewer. */
  data: ArrayBuffer | null
}

export function useAttachments() {
  const viewed = useState<Viewed | null>('attachment-viewer', () => null)
  const opening = useState<string | null>('attachment-opening', () => null)
  const showError = useErrorToast()
  const toast = useToast()

  /** `report`: the inspection already made (null: it failed); left out, it is done here first. */
  async function download(messageId: number, index: number, _filename: string, report?: AttachmentReport | null) {
    if (report === undefined) report = await api.inspectAttachment(messageId, index).catch(() => null)
    if (report?.trust.level === 'low') {
      const why = report.trust.reasons.filter(r => !r.positive).map(r => `• ${r.text}`).join('\n')
      const ok = await confirm(`${why}\n\n${tr('common.attachment.saveAnyway')}`, {
        title: tr('common.attachment.suspicious'),
        kind: 'warning'
      })
      if (!ok) return
    }
    try {
      // Warned here already: the engine does not ask a second time.
      const saved = await api.saveAttachment(messageId, index, report?.trust.level === 'low')
      if (!saved) return
      // A click on the toast, or its button, opens the file with the system's program (not a
      // program or a suspicious file: the engine refuses those).
      const open = () => api.openSaved(saved.path).catch(showError)
      toast.add({
        title: tr('common.attachment.saved'),
        description: saved.path,
        icon: 'i-lucide-check',
        ...(saved.openable && {
          onClick: open,
          class: 'cursor-pointer',
          actions: [{ label: tr('common.attachment.open'), icon: 'i-lucide-external-link', color: 'neutral', variant: 'outline', onClick: open }]
        })
      })
    } catch (e) {
      showError(e)
    }
  }

  /** `ask`: where to save it is chosen; otherwise the Downloads folder. */
  async function saveImage(messageId: number, src: string, ask: boolean) {
    try {
      const saved = await api.saveImage(messageId, src, ask)
      if (!saved) return
      const open = () => api.openSaved(saved.path).catch(showError)
      const reveal = () => revealItemInDir(saved.path).catch(showError)
      toast.add({
        title: tr('common.attachment.imageSaved'),
        description: saved.path,
        icon: 'i-lucide-check',
        ...(saved.openable && { onClick: open, class: 'cursor-pointer' }),
        actions: [
          ...(saved.openable ? [{ label: tr('common.attachment.open'), icon: 'i-lucide-external-link', color: 'neutral' as const, variant: 'outline' as const, onClick: open }] : []),
          { label: tr('common.attachment.showInFolder'), icon: 'i-lucide-folder-open', color: 'neutral' as const, variant: 'outline' as const, onClick: reveal }
        ]
      })
    } catch (e) {
      showError(e, tr('common.attachment.imageFailed'))
    }
  }

  async function view(messageId: number, index: number, filename: string, mime: string) {
    const key = `${messageId}:${index}`
    opening.value = key
    try {
      const { mime: sniffed, data } = await api.attachmentView(messageId, index)
      close()
      // The content's real type decides how it is shown, not the type the sender declared.
      mime = sniffed || mime
      const blob = new Blob([data], { type: mime })
      viewed.value = {
        messageId,
        index,
        filename,
        mime,
        url: mime.startsWith('image/') ? URL.createObjectURL(blob) : '',
        text: mime === 'text/plain' ? await blob.text() : null,
        data: mime === 'application/pdf' ? data : null
      }
    } catch (e) {
      showError(e, tr('common.attachment.previewFailed'))
    } finally {
      if (opening.value === key) opening.value = null
    }
  }

  function close() {
    if (viewed.value?.url) URL.revokeObjectURL(viewed.value.url)
    viewed.value = null
  }

  return { viewed, opening, download, saveImage, view, close }
}
