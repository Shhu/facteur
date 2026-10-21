export interface MailtoFields {
  to: string[]
  cc: string[]
  bcc: string[]
  subject: string
  body: string
}

/** Keeps '+' literal: RFC 6068 is not form encoding. */
function decode(s: string): string {
  try {
    return decodeURIComponent(s)
  } catch {
    return s
  }
}

export function parseMailto(link: string): MailtoFields {
  const rest = link.replace(/^mailto:/i, '')
  const q = rest.indexOf('?')
  const addresses = q < 0 ? rest : rest.slice(0, q)
  const query = q < 0 ? '' : rest.slice(q + 1)
  const params: [string, string][] = query.split('&').filter(Boolean).map((pair) => {
    const eq = pair.indexOf('=')
    return eq < 0 ? [decode(pair), ''] : [decode(pair.slice(0, eq)), decode(pair.slice(eq + 1))]
  })
  const split = (v: string | null) => (v ?? '').split(',').map(s => s.trim()).filter(Boolean)
  const get = (k: string) => params.find(([key]) => key.toLowerCase() === k)?.[1] ?? null
  return {
    to: split(decode(addresses)),
    cc: split(get('cc')),
    bcc: split(get('bcc')),
    subject: get('subject') ?? '',
    body: get('body') ?? ''
  }
}
