import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

/** Every .vue file under `dir`. */
function vueFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return vueFiles(path)
    return path.endsWith('.vue') ? [path] : []
  })
}

describe('template event handlers', () => {
  it('never call a function that returns the handler', () => {
    // `@update="htmlFor(session)"` runs htmlFor at each event and drops the handler it returns:
    // the composer's text never reached the draft that way.
    const found: string[] = []
    for (const file of vueFiles(join(__dirname, '..', 'app'))) {
      const source = readFileSync(file, 'utf8')
      const curried = [...source.matchAll(/const (\w+) = \([^)]*\)\s*=>\s*\(/g)].map(m => m[1]!)
      for (const name of curried) {
        if (new RegExp(`@[\\w:.-]+="${name}\\(`).test(source)) found.push(`${file}: ${name}`)
      }
    }
    expect(found).toEqual([])
  })
})
