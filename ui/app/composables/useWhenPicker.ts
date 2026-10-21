export interface WhenRequest {
  kind: 'snooze' | 'send'
  title: string
}

let resolver: ((at: number | null) => void) | null = null

export function useWhenPicker() {
  const request = useState<WhenRequest | null>('when-picker', () => null)

  /** `null` if the user cancels. */
  function ask(r: WhenRequest): Promise<number | null> {
    resolver?.(null)
    request.value = r
    return new Promise((resolve) => {
      resolver = resolve
    })
  }

  function answer(at: number | null) {
    request.value = null
    resolver?.(at)
    resolver = null
  }

  return { request, ask, answer }
}
