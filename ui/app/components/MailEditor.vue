<script setup lang="ts">
import type { Editor } from '@tiptap/vue-3'
import type { DropdownMenuItem, EditorCustomHandlers, EditorToolbarItem } from '@nuxt/ui'

const html = defineModel<string>({ required: true })
/** What UEditor is given: the model as it comes from outside. In the composer it is written back
 *  only by `flush`, so typing does not turn a long quoted thread into HTML at every key. */
const shown = ref(html.value)
watch(html, (v) => {
  if (v !== written) shown.value = v
})
/** The HTML this editor wrote out last (its own echo is not content to set again). */
let written: string | null = null
const props = withDefaults(defineProps<{
  /** `start`: cursor at the start (reply, above the quote). */
  autofocus?: 'start' | 'end' | boolean
  placeholder?: string
  /** Signature editor: shorter, toolbar limited to text, links and images. */
  compact?: boolean
  /** Wider images are shrunk on insertion (signature logo). */
  maxImageWidth?: number
  touch?: boolean
  composer?: boolean
  moreItems?: DropdownMenuItem[][]
  readonly?: boolean
  /** Largest message once encoded, in bytes: an image that would make it larger is refused. */
  maxBytes?: number
  /** Bytes the message already takes outside the editor (attachments). */
  usedBytes?: number
}>(), { autofocus: false, placeholder: undefined, compact: false, maxImageWidth: undefined, touch: false, composer: false, moreItems: () => [], readonly: false, maxBytes: undefined, usedBytes: 0 })
/** `files`: non-image files dropped on the editor (to attach). `changed`: an edit, in the
 *  composer, before its HTML and text are written out (see `flush`). */
const emit = defineEmits<{ text: [value: string], files: [files: File[]], changed: [] }>()
const { t } = useI18n()
const toast = useToast()

const fileInput = useTemplateRef('fileInput')
/** Editor targeted by the toolbar's last action (link, image). */
let target: Editor | null = null

// A small dialog rather than the browser's prompt().

const linkOpen = ref(false)
const linkUrl = ref('')

function applyLink() {
  const href = linkUrl.value.trim()
  if (target && href) {
    const url = /^[a-z][a-z0-9+.-]*:/i.test(href) ? href : `https://${href}`
    target.chain().focus().extendMarkRange('link').setLink({ href: url }).run()
  }
  linkOpen.value = false
}

/** Shrinks a too-wide image (PNG kept for logo transparency, else JPEG). */
async function shrink(src: string, type: string): Promise<string> {
  const max = props.maxImageWidth
  if (!max || type === 'image/svg+xml' || type === 'image/gif') return src
  const img = new Image()
  img.src = src
  await img.decode()
  if (img.naturalWidth <= max) return src
  const canvas = document.createElement('canvas')
  canvas.width = max
  canvas.height = Math.round(img.naturalHeight * max / img.naturalWidth)
  canvas.getContext('2d')?.drawImage(img, 0, 0, canvas.width, canvas.height)
  return type === 'image/png' ? canvas.toDataURL('image/png') : canvas.toDataURL('image/jpeg', 0.9)
}

/** What is used of the editor (the toolbar handlers get the full Tiptap instance). */
type EditorLike = Pick<Editor, 'chain' | 'view' | 'commands'>

/** Display width of a signature image: small, with enough pixels for high-density screens
 *  (the handles enlarge it). */
const SIGNATURE_IMAGE_WIDTH = 160

async function insertFiles(files: File[], editor: Pick<Editor, 'chain'> | null) {
  for (const file of files) {
    let src = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader()
      reader.onload = () => resolve(String(reader.result))
      reader.onerror = () => reject(reader.error)
      reader.readAsDataURL(file)
    })
    // Signature: a single image goes through the cropper first.
    if (props.compact && files.length === 1 && file.type !== 'image/svg+xml' && file.type !== 'image/gif') {
      const cropped = await askCrop(src)
      if (!cropped) return
      src = cropped
    }
    const type = src.slice(5, src.indexOf(';'))
    src = await shrink(src, type)
    // Embedded in the message: over the size limit, the server would refuse it.
    if (props.maxBytes && props.usedBytes + html.value.length + src.length > props.maxBytes) {
      toast.add({ title: t('app.composer.tooLarge', { name: file.name, max: Math.round(props.maxBytes / 1024 / 1024) }), color: 'warning', icon: 'i-lucide-file-warning' })
      continue
    }
    editor?.chain().focus().setImage({ src, alt: file.name, ...(props.compact ? await displaySize(src) : {}) }).run()
  }
}

async function displaySize(src: string): Promise<{ width?: number, height?: number }> {
  const img = new Image()
  img.src = src
  await img.decode()
  if (img.naturalWidth <= SIGNATURE_IMAGE_WIDTH) return {}
  return { width: SIGNATURE_IMAGE_WIDTH, height: Math.round(img.naturalHeight * SIGNATURE_IMAGE_WIDTH / img.naturalWidth) }
}

const cropSrc = ref('')
const cropOpen = ref(false)
let cropResolve: ((src: string | null) => void) | null = null

function askCrop(src: string): Promise<string | null> {
  cropResolve?.(null)
  cropSrc.value = src
  cropOpen.value = true
  return new Promise((resolve) => {
    cropResolve = resolve
  })
}
function cropDone(src: string) {
  cropResolve?.(src)
  cropResolve = null
}
// Closed without confirming: cancelled (`done` fires before the close).
watch(cropOpen, (isOpen) => {
  if (isOpen) return
  cropResolve?.(null)
  cropResolve = null
})

/** Only embedded images can be cropped: a remote one (quoted reply) taints the canvas, which
 *  then refuses to be read (SecurityError). */
function croppable(editor: Editor): boolean {
  if (!editor.isActive('image')) return false
  const { src } = editor.getAttributes('image') as { src?: string }
  return !!src?.startsWith('data:')
}

/** Crops the selected image, keeping its displayed width. */
async function cropSelected(editor: Editor) {
  const pos = editor.state.selection.from
  const { src, width } = editor.getAttributes('image') as { src?: string, width?: number | null }
  if (!src?.startsWith('data:')) return
  const cropped = await askCrop(src)
  if (!cropped || cropped === src) return
  const img = new Image()
  img.src = cropped
  await img.decode()
  const w = width ? Math.min(width, img.naturalWidth) : null
  editor.chain().focus().setNodeSelection(pos)
    .updateAttributes('image', { src: cropped, width: w, height: w ? Math.round(img.naturalHeight * w / img.naturalWidth) : null })
    .run()
}

async function insertImages(e: Event) {
  const input = e.target as HTMLInputElement
  await insertFiles(Array.from(input.files ?? []), target)
  input.value = ''
}

/** Target of pasted or dropped files: images go into the text, others become attachments. */
let current: EditorLike | null = null
const isImage = (f: File) => f.type.startsWith('image/')

// UEditor (Nuxt UI 4.11) passes its own onCreate/onUpdate to Tiptap after the forwarded props,
// so `@create` / `@update` on it never fire: the Tiptap instance it exposes is listened to
// directly instead (plain-text version of the mail, target of pasted and dropped images).
const editorRef = useTemplateRef<{ editor?: Editor }>('editorRef')
const emitText = (e: Pick<Editor, 'getText'>) => emit('text', e.getText({ blockSeparator: '\n' }))
/** An edit not yet written out (composer). */
let pending = false
let idle: ReturnType<typeof setTimeout> | undefined
/** The HTML and the plain text written out now, if an edit waits. */
function flush() {
  clearTimeout(idle)
  const e = editorRef.value?.editor
  if (!pending || !e) return
  pending = false
  written = e.getHTML()
  html.value = written
  emitText(e)
}
watch(() => editorRef.value?.editor, (e, _, onCleanup) => {
  current = e ?? null
  if (!e) return
  if (props.readonly) e.setEditable(false)
  emitText(e)
  let onUpdate: () => void
  if (props.composer) {
    // UEditor's own listener (the editor's `onUpdate` option) writes the whole HTML at every
    // key: taken off alone, the extensions keep theirs. The edit is only noted, and written out
    // after a pause or when the composer asks (save, send, close).
    e.off('update', e.options.onUpdate)
    onUpdate = () => {
      pending = true
      emit('changed')
      clearTimeout(idle)
      idle = setTimeout(flush, 700)
    }
  } else {
    // A signature: short, written out by UEditor itself (v-model).
    onUpdate = () => emitText(e)
  }
  e.on('update', onUpdate)
  onCleanup(() => {
    e.off('update', onUpdate)
    clearTimeout(idle)
    if (current === e) current = null
  })
}, { immediate: true })

/** Inserts HTML at the cursor (a template), as a typed edit. */
defineExpose({
  insert(content: string) {
    current?.chain().focus().insertContent(content).run()
  },
  flush
})

// Read-only while the message is being sent: what is sent is what is shown.
watch(() => props.readonly, (readonly) => {
  const e = editorRef.value?.editor
  if (e && e.isEditable === readonly) e.setEditable(!readonly, false)
})

function onPaste(e: ClipboardEvent) {
  const images = Array.from(e.clipboardData?.files ?? []).filter(isImage)
  if (!images.length || props.readonly) return
  e.preventDefault()
  e.stopPropagation()
  insertFiles(images, current)
}

function onDrop(e: DragEvent) {
  const files = Array.from(e.dataTransfer?.files ?? [])
  if (!files.length) return
  e.preventDefault()
  e.stopPropagation()
  if (props.readonly) return
  if (current && e.dataTransfer) {
    // The image lands where it is dropped.
    const pos = current.view.posAtCoords({ left: e.clientX, top: e.clientY })
    if (pos) current.commands.setTextSelection(pos.pos)
  }
  insertFiles(files.filter(isImage), current)
  const others = files.filter(f => !isImage(f))
  if (others.length) emit('files', others)
}

const handlers = {
  editLink: {
    canExecute: () => true,
    execute: (editor: Editor) => {
      target = editor
      if (editor.isActive('link')) return editor.chain().focus().extendMarkRange('link').unsetLink()
      linkUrl.value = ''
      linkOpen.value = true
      return editor.chain()
    },
    isActive: (editor: Editor) => editor.isActive('link'),
    isDisabled: (editor: Editor) => editor.state.selection.empty && !editor.isActive('link')
  },
  insertImage: {
    canExecute: () => true,
    execute: (editor: Editor) => {
      target = editor
      fileInput.value?.click()
      return editor.chain()
    },
    isActive: () => false
  },
  cropImage: {
    canExecute: () => true,
    execute: (editor: Editor) => {
      cropSelected(editor)
      return editor.chain()
    },
    isActive: () => false,
    isDisabled: (editor: Editor) => !croppable(editor)
  },
  textBigger: {
    canExecute: (editor: Editor) => canStepFontSize(editor, 1),
    execute: (editor: Editor) => stepFontSize(editor, 1),
    isActive: () => false
  },
  textSmaller: {
    canExecute: (editor: Editor) => canStepFontSize(editor, -1),
    execute: (editor: Editor) => stepFontSize(editor, -1),
    isActive: () => false
  },
  columns2: {
    canExecute: () => true,
    execute: (editor: Editor) => makeColumns(editor, 2),
    isActive: () => false,
    isDisabled: (editor: Editor) => hasColumns(editor)
  },
  columns3: {
    canExecute: () => true,
    execute: (editor: Editor) => makeColumns(editor, 3),
    isActive: () => false,
    isDisabled: (editor: Editor) => hasColumns(editor)
  },
  addColumn: {
    canExecute: () => true,
    execute: (editor: Editor) => addColumn(editor),
    isActive: () => false,
    isDisabled: (editor: Editor) => !currentColumn(editor)
  },
  columnDivider: {
    canExecute: () => true,
    execute: (editor: Editor) => toggleDivider(editor),
    isActive: (editor: Editor) => !!currentColumn(editor)?.node.attrs.divider,
    isDisabled: (editor: Editor) => !currentColumn(editor)
  },
  removeColumn: {
    canExecute: () => true,
    execute: (editor: Editor) => removeColumn(editor),
    isActive: () => false,
    isDisabled: (editor: Editor) => !currentColumn(editor)
  },
  unwrapColumns: {
    canExecute: () => true,
    execute: (editor: Editor) => unwrapColumns(editor),
    isActive: () => false,
    isDisabled: (editor: Editor) => !currentColumn(editor)
  }
} satisfies EditorCustomHandlers

const fullToolbar = computed<EditorToolbarItem<typeof handlers>[][]>(() => [
  [
    { kind: 'mark', mark: 'bold', icon: 'i-lucide-bold', tooltip: { text: t('app.editor.bold'), kbds: ['meta', 'b'] } },
    { kind: 'mark', mark: 'italic', icon: 'i-lucide-italic', tooltip: { text: t('app.editor.italic'), kbds: ['meta', 'i'] } },
    { kind: 'mark', mark: 'underline', icon: 'i-lucide-underline', tooltip: { text: t('app.editor.underline'), kbds: ['meta', 'u'] } },
    { kind: 'mark', mark: 'strike', icon: 'i-lucide-strikethrough', tooltip: { text: t('app.editor.strike') } },
    { kind: 'textBigger', icon: 'i-lucide-a-arrow-up', tooltip: { text: t('app.editor.textBigger') } },
    { kind: 'textSmaller', icon: 'i-lucide-a-arrow-down', tooltip: { text: t('app.editor.textSmaller') } }
  ],
  [
    { kind: 'bulletList', icon: 'i-lucide-list', tooltip: { text: t('app.editor.bulletList') } },
    { kind: 'orderedList', icon: 'i-lucide-list-ordered', tooltip: { text: t('app.editor.orderedList') } },
    { kind: 'blockquote', icon: 'i-lucide-text-quote', tooltip: { text: t('app.editor.quote') } }
  ],
  [
    { kind: 'editLink', icon: 'i-lucide-link', tooltip: { text: t('app.editor.link') } },
    { kind: 'insertImage', icon: 'i-lucide-image', tooltip: { text: t('app.editor.image') } },
    { kind: 'cropImage', icon: 'i-lucide-crop', tooltip: { text: t('app.editor.crop') } }
  ],
  [
    { kind: 'clearFormatting', icon: 'i-lucide-remove-formatting', tooltip: { text: t('app.editor.clearFormatting') } },
    { kind: 'undo', icon: 'i-lucide-undo-2', tooltip: { text: t('app.editor.undo'), kbds: ['meta', 'z'] } },
    { kind: 'redo', icon: 'i-lucide-redo-2', tooltip: { text: t('app.editor.redo'), kbds: ['meta', 'shift', 'z'] } }
  ]
])
/** Signature columns: layout, then the cursor's column. */
const columnsGroup = computed<EditorToolbarItem<typeof handlers>[]>(() => [{
  icon: 'i-lucide-columns-3',
  tooltip: { text: t('app.editor.columns') },
  content: { align: 'start' },
  items: [
    [
      { kind: 'columns2', icon: 'i-lucide-columns-2', label: t('app.editor.columns2') },
      { kind: 'columns3', icon: 'i-lucide-columns-3', label: t('app.editor.columns3') }
    ],
    [
      { kind: 'addColumn', icon: 'i-lucide-between-vertical-start', label: t('app.editor.addColumn') },
      { kind: 'columnDivider', icon: 'i-lucide-separator-vertical', label: t('app.editor.divider') },
      { kind: 'removeColumn', icon: 'i-lucide-trash-2', label: t('app.editor.removeColumn') },
      { kind: 'unwrapColumns', icon: 'i-lucide-rows-3', label: t('app.editor.unwrapColumns') }
    ],
    [{ type: 'label', label: t('app.editor.columnHint') }]
  ]
}])

const touchToolbar = computed<EditorToolbarItem<typeof handlers>[][]>(() => {
  const [text, blocks, media, history] = fullToolbar.value as [EditorToolbarItem<typeof handlers>[], EditorToolbarItem<typeof handlers>[], EditorToolbarItem<typeof handlers>[], EditorToolbarItem<typeof handlers>[]]
  const labelled = (items: EditorToolbarItem<typeof handlers>[]) => items.map(i => ({ ...i, label: i.tooltip?.text, tooltip: undefined })) as EditorToolbarItem<typeof handlers>[]
  return [[
    text[0]!, text[1]!, text[2]!, blocks[0]!, media[0]!, media[1]!, history[1]!,
    {
      icon: 'i-lucide-ellipsis',
      tooltip: { text: t('app.editor.more') },
      content: { align: 'end', side: 'top' },
      items: [
        labelled([text[3]!, blocks[1]!, blocks[2]!]),
        labelled([text[4]!, text[5]!]),
        labelled([media[2]!, history[0]!, history[2]!]),
        ...(props.moreItems as EditorToolbarItem<typeof handlers>[][])
      ]
    }
  ]]
})

/** Signature: no lists, quote or undo button (the shortcut stays); columns instead. */
const toolbar = computed(() => {
  const full = fullToolbar.value
  if (props.composer && props.touch) return touchToolbar.value
  return props.compact ? [full[0]!, full[2]!, columnsGroup.value, [full[3]![0]!]] : full
})
</script>

<template>
  <div
    class="flex flex-col"
    :class="[compact ? '' : 'flex-1', composer ? (touch ? 'shrink-0' : 'min-h-0') : 'min-h-0 rounded-md border border-default']"
    @paste.capture="onPaste"
    @dragover.capture="(e: DragEvent) => { if (e.dataTransfer?.types.includes('Files')) e.preventDefault() }"
    @drop.capture="onDrop"
  >
    <!-- Composer: the toolbar comes first (desktop band) or last (phone, stuck above the
         keyboard); the `footer` slot goes after the text, inside the same scroller. -->
    <UEditor
      v-if="composer"
      ref="editorRef"
      v-slot="{ editor }"
      :model-value="shown"
      content-type="html"
      :autofocus="autofocus"
      :handlers="handlers"
      :image="false"
      :extensions="editorExtensions"
      :placeholder="placeholder ?? $t('app.editor.placeholder')"
      class="flex flex-1 flex-col"
      :class="touch ? 'min-h-56' : 'min-h-0 overflow-y-auto'"
      :ui="{
        content: 'flex h-auto flex-1 flex-col',
        base: touch
          ? 'min-h-40 flex-1 px-4.5 py-4 text-base text-highlighted sm:px-4.5 [&_p]:leading-[1.6]'
          : 'min-h-40 flex-1 px-7 py-4.5 text-[15px] text-highlighted sm:px-7 [&_p]:leading-[1.65]'
      }"
    >
      <div
        v-if="touch"
        class="sticky bottom-0 z-10 order-last border-t border-default bg-(--surface-side) px-2 pt-1.5 pb-[max(0.875rem,env(safe-area-inset-bottom))]"
      >
        <UEditorToolbar
          :editor="editor"
          :items="toolbar"
          size="lg"
          :ui="{ base: 'gap-0' }"
          class="[&_button]:size-11 [&_button]:shrink-0 [&_button]:justify-center [&_button]:rounded-[10px]"
        />
      </div>
      <div v-else class="sticky top-0 z-10 order-first bg-(--surface-card) px-6 pt-2.5">
        <UEditorToolbar
          :editor="editor"
          :items="toolbar"
          size="sm"
          :ui="{ base: 'gap-1 flex-wrap', separator: 'my-1.75' }"
          class="rounded-xl border border-default bg-(--surface-side) p-1 [&_button]:size-8 [&_button]:justify-center [&_button]:rounded-lg"
        />
      </div>
      <div v-if="$slots.footer" class="order-last">
        <slot name="footer" />
      </div>
    </UEditor>
    <UEditor
      v-else
      ref="editorRef"
      v-slot="{ editor }"
      v-model="html"
      content-type="html"
      :autofocus="autofocus"
      :handlers="handlers"
      :image="false"
      :extensions="editorExtensions"
      :placeholder="placeholder ?? $t('app.editor.placeholder')"
      class="flex flex-1 flex-col"
      :class="compact ? 'min-h-28' : 'min-h-64'"
      :ui="{ base: compact ? 'min-h-24 px-3 py-2 sm:px-3' : 'min-h-64 px-4 py-3 sm:px-4' }"
    >
      <UEditorToolbar
        :editor="editor"
        :items="toolbar"
        :size="touch ? 'lg' : 'sm'"
        class="sticky top-0 z-10 border-b border-default bg-default px-1 py-1"
        :class="touch ? 'overflow-x-auto [&_button]:size-10 [&_button]:shrink-0 [&_button]:justify-center' : ''"
      />
    </UEditor>
    <ImageCropper v-model:open="cropOpen" :src="cropSrc" @done="cropDone" />
    <input ref="fileInput" type="file" accept="image/*" multiple class="hidden" @change="insertImages">

    <UModal v-model:open="linkOpen" :title="$t('app.editor.insertLink')" :ui="{ content: 'max-w-md' }">
      <template #body>
        <form class="flex gap-2" @submit.prevent="applyLink">
          <UInput v-model="linkUrl" :placeholder="$t('app.editor.linkPlaceholder')" autofocus class="flex-1" />
          <UButton type="submit" :label="$t('app.editor.insert')" />
        </form>
      </template>
    </UModal>
  </div>
</template>
