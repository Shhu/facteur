const DOMAIN = /^[a-z0-9-]+(\.[a-z0-9-]+)+$/

/** CSP sources a message's remote content may load from: all of it once shown, else only the
 *  approved domains (with their subdomains). Anything not a plain domain is left out. */
export function remoteSources(all: boolean, hosts: string[]): string {
  if (all) return ' https: http:'
  return hosts
    .filter(h => DOMAIN.test(h))
    .map(h => ` https://${h} https://*.${h} http://${h} http://*.${h}`)
    .join('')
}
