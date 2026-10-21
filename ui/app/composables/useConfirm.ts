/** Same signature as the dialog plugin's `confirm`, shown in ConfirmDialog (app.vue). */

export interface ConfirmOptions {
  title?: string
  /** `warning`: red OK button. */
  kind?: 'info' | 'warning' | 'error'
  okLabel?: string
  cancelLabel?: string
}

interface Request extends ConfirmOptions {
  message: string
  resolve: (ok: boolean) => void
}

/** One at a time: a new question answers the previous one with cancel. */
export const confirmRequest = shallowRef<Request | null>(null)

export function confirm(message: string, options: ConfirmOptions = {}): Promise<boolean> {
  confirmRequest.value?.resolve(false)
  return new Promise((resolve) => {
    confirmRequest.value = { ...options, message, resolve }
  })
}

export function answerConfirm(ok: boolean) {
  const request = confirmRequest.value
  confirmRequest.value = null
  request?.resolve(ok)
}
