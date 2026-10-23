/** Dictation: the microphone recorded at 16 kHz, then turned into text by the local model (the
 *  sound never leaves the computer). Five minutes at most per recording. */
/** "fr" or "en"; anything else: found by the model. */
export const DICTATION_LANGUAGE = 'dictation_language'

export function useDictation() {
  const recording = ref(false)
  const transcribing = ref(false)
  let stream: MediaStream | null = null
  let context: AudioContext | null = null
  let node: ScriptProcessorNode | null = null
  let chunks: Float32Array[] = []
  let length = 0
  let limit: ReturnType<typeof setTimeout> | undefined
  const MAX_SECONDS = 300

  /** Asking for the microphone: a second click meanwhile starts nothing more. */
  let starting = false
  /** Bumped by each release: a start still waiting for the microphone then lets it go. */
  let generation = 0

  async function start(onLimit: () => void) {
    if (recording.value || transcribing.value || starting) return
    starting = true
    const asked = ++generation
    let media: MediaStream
    try {
      media = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } })
    } finally {
      starting = false
    }
    // Stopped, cancelled or closed while the microphone was being opened.
    if (asked !== generation) {
      media.getTracks().forEach(t => t.stop())
      return
    }
    stream = media
    let source: MediaStreamAudioSourceNode
    try {
      context = new AudioContext({ sampleRate: 16000 })
      source = context.createMediaStreamSource(stream)
      node = context.createScriptProcessor(4096, 1, 1)
    } catch (e) {
      // The microphone is not left open behind an idle button.
      release()
      throw e
    }
    // Its own buffer: a processor left over could not add to the next recording.
    const own: Float32Array[] = []
    chunks = own
    length = 0
    node.onaudioprocess = (e) => {
      if (chunks !== own) return
      const data = new Float32Array(e.inputBuffer.getChannelData(0))
      own.push(data)
      length += data.length
    }
    source.connect(node)
    node.connect(context.destination)
    recording.value = true
    clearTimeout(limit)
    limit = setTimeout(onLimit, MAX_SECONDS * 1000)
  }

  function release() {
    generation++
    clearTimeout(limit)
    node?.disconnect()
    stream?.getTracks().forEach(t => t.stop())
    context?.close().catch(() => {})
    node = null
    stream = null
    context = null
    recording.value = false
  }

  /** The text said since `start`; empty when nothing was heard. */
  async function stop(): Promise<string> {
    if (!recording.value) return ''
    release()
    // 16-bit little-endian, as the engine reads it.
    const pcm = new Uint8Array(length * 2)
    const view = new DataView(pcm.buffer)
    let offset = 0
    for (const chunk of chunks) {
      for (const s of chunk) {
        view.setInt16(offset, Math.max(-1, Math.min(1, s)) * 0x7fff, true)
        offset += 2
      }
    }
    chunks = []
    if (length < 16000 / 2) return ''
    transcribing.value = true
    try {
      // The language spoken, not the interface's: forced to another one, the model translates.
      const language = await api.getSetting(DICTATION_LANGUAGE).catch(() => null)
      return await api.aiTranscribe(pcm, language === 'fr' || language === 'en' ? language : '')
    } finally {
      transcribing.value = false
    }
  }

  onScopeDispose(release)
  return { recording, transcribing, start, stop, cancel: release }
}
