import type { CommandTiming, PerfRecord } from '~/composables/usePerf'
import { dateFormat, intlLocale, tr } from './format'

export function msLabel(ms: number): string {
  return ms.toLocaleString(intlLocale(), { maximumFractionDigits: ms < 10 ? 1 : 0 })
}

export function num(n: number): string {
  return n.toLocaleString(intlLocale())
}

export function percent(share: number): string {
  return share.toLocaleString(intlLocale(), { style: 'percent', maximumFractionDigits: 0 })
}

export function spanLabel(secs: number): string {
  const s = Math.max(0, Math.round(secs))
  if (s < 60) return tr('app.diagnostics.span.s', { n: s })
  if (s < 3600) return tr('app.diagnostics.span.min', { m: Math.floor(s / 60), s: String(s % 60).padStart(2, '0') })
  if (s < 86400) return tr('app.diagnostics.span.h', { h: Math.floor(s / 3600), m: String(Math.floor(s / 60) % 60).padStart(2, '0') })
  return tr('app.diagnostics.span.d', { d: Math.floor(s / 86400), h: Math.floor(s / 3600) % 24 })
}

export function durationLabel(ms: number): string {
  if (ms < 1000) return `${msLabel(ms)} ms`
  if (ms < 60_000) return tr('app.diagnostics.span.s', { n: (ms / 1000).toLocaleString(intlLocale(), { maximumFractionDigits: 1 }) })
  return spanLabel(ms / 1000)
}

export function agoMs(at: number, now = Date.now()): string {
  return tr('app.diagnostics.ago', { span: spanLabel((now - at) / 1000) })
}

export function clock(at: number, millis = false): string {
  const f = dateFormat({ hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false, ...(millis ? { fractionalSecondDigits: 3 } : {}) })
  return f.format(new Date(at))
}

export function tone(ms: number): string {
  return ms > 500 ? 'text-error' : ms > 150 ? 'text-warning' : 'text-muted'
}

export interface CommandStat {
  command: string
  n: number
  failed: number
  p50: number
  p95: number
  max: number
  last: number
  lastAt: number
}

export function commandStats(calls: CommandTiming[]): CommandStat[] {
  const by = new Map<string, CommandTiming[]>()
  for (const c of calls) {
    const list = by.get(c.command)
    if (list) list.push(c)
    else by.set(c.command, [c])
  }
  return [...by.entries()].map(([command, list]) => {
    const sorted = list.map(c => c.ms).sort((a, b) => a - b)
    const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))]!
    const last = list.at(-1)!
    return { command, n: sorted.length, failed: list.filter(c => !c.ok).length, p50: at(0.5), p95: at(0.95), max: sorted.at(-1)!, last: last.ms, lastAt: last.at }
  })
}

export interface EngineGroup {
  key: string
  kind: PerfRecord['kind']
  what: string
  count: number
  total: number
  max: number
  lastAt: number
  /** Newest first. */
  events: PerfRecord[]
}

export function engineGroups(records: PerfRecord[]): EngineGroup[] {
  const by = new Map<string, EngineGroup>()
  for (const e of records) {
    const key = `${e.kind}\u0000${e.what}`
    let g = by.get(key)
    if (!g) {
      g = { key, kind: e.kind, what: e.what, count: 0, total: 0, max: 0, lastAt: 0, events: [] }
      by.set(key, g)
    }
    g.count++
    g.total += e.ms
    g.max = Math.max(g.max, e.ms)
    g.lastAt = Math.max(g.lastAt, e.at)
    g.events.unshift(e)
  }
  return [...by.values()]
}
