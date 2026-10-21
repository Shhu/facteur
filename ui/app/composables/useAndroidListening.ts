/** Injected by the Android app (MainActivity); absent elsewhere. A web message listener object:
 *  requests and answers are JSON strings, answers arrive asynchronously through `onmessage`. */
interface FacteurAndroid {
  postMessage(message: string): void
  onmessage: ((event: { data: string }) => void) | null
}

interface Answer { id: number, enabled: boolean }

const pending = new Map<number, (enabled: boolean) => void>()
let nextId = 1

function bridge(): FacteurAndroid | undefined {
  return (globalThis as { FacteurAndroid?: FacteurAndroid }).FacteurAndroid
}

function request(b: FacteurAndroid, op: 'get' | 'set', enabled?: boolean): Promise<boolean> {
  if (!b.onmessage) {
    b.onmessage = (event) => {
      let answer: Answer
      try {
        answer = JSON.parse(event.data) as Answer
      } catch {
        return
      }
      pending.get(answer.id)?.(answer.enabled)
      pending.delete(answer.id)
    }
  }
  const id = nextId++
  return new Promise((resolve) => {
    pending.set(id, resolve)
    b.postMessage(JSON.stringify(enabled === undefined ? { id, op } : { id, op, enabled }))
  })
}

/** Background listening on Android (a foreground service). `supported` is false on desktop,
 *  where the app always listens. */
export function useAndroidListening() {
  const b = bridge()
  const enabled = useState('android-listening', () => false)
  const loaded = useState('android-listening-loaded', () => false)
  if (b && !loaded.value) {
    loaded.value = true
    request(b, 'get').then((value) => { enabled.value = value })
  }

  function set(value: boolean) {
    enabled.value = value
    if (b) request(b, 'set', value).then((actual) => { enabled.value = actual })
  }

  return { supported: !!b, enabled, set }
}
