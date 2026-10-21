import { useLocalStorage } from '@vueuse/core'
import { effectScope } from 'vue'
import type { Ref } from 'vue'

export type ThreadOrder = 'oldest' | 'newest'

let order: Ref<ThreadOrder> | undefined

export function useThreadOrder() {
  order ??= effectScope(true).run(() => useLocalStorage<ThreadOrder>('facteur:thread-order', 'oldest', { writeDefaults: false }))!
  return { order }
}
