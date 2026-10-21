import { useLocalStorage } from '@vueuse/core'
import { effectScope } from 'vue'
import type { Ref } from 'vue'
import type { Mailbox } from '~/utils/types'

export type FolderCountMode = 'unread' | 'total'
export type FolderCountUnit = 'conversations' | 'messages'

let state: { mode: Ref<FolderCountMode>, unit: Ref<FolderCountUnit> } | undefined

/** Per-device settings; one shared state, kept after the caller unmounts. */
export function useFolderCounts() {
  state ??= effectScope(true).run(() => ({
    mode: useLocalStorage<FolderCountMode>('facteur:folder-counts', 'unread', { writeDefaults: false }),
    unit: useLocalStorage<FolderCountUnit>('facteur:folder-count-unit', 'conversations', { writeDefaults: false })
  }))!
  const { mode, unit } = state

  const unreadOf = (m: Mailbox) => (unit.value === 'conversations' ? m.unread_threads ?? m.unread : m.unread)

  /** Messages: the server's count when known (the cache may only hold the recent part of a large
   *  folder). Conversations: only the cache's are known; `partial` when the server holds more. */
  function totalOf(m: Mailbox): { n: number, partial: boolean } {
    if (unit.value === 'messages') return { n: m.server_total ?? m.total, partial: false }
    return { n: m.threads ?? m.total, partial: m.server_total !== null && m.server_total > m.total }
  }

  return { mode, unit, unreadOf, totalOf }
}
