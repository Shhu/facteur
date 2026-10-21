import { accountColorName } from '~/utils/accountColors'

export function useAccountOrder() {
  const { accounts, loadAccounts } = useMail()
  const showError = useErrorToast()

  async function moveTo(id: number, index: number) {
    const list = accounts.value
    const order = list.map(a => a.id).filter(x => x !== id)
    order.splice(Math.max(0, Math.min(index, order.length)), 0, id)
    if (order.every((x, i) => x === list[i]?.id)) return
    try {
      // Rank-based colours would shift with the rank: pin the ones shown now.
      for (const a of list) {
        if (!a.color) await api.setAccountColor(a.id, accountColorName(a, list))
      }
      await api.reorderAccounts(order)
      await loadAccounts()
    } catch (e) {
      showError(e)
    }
  }

  function moveBy(id: number, delta: number) {
    const index = accounts.value.findIndex(a => a.id === id)
    if (index >= 0) return moveTo(id, index + delta)
  }

  return { moveTo, moveBy }
}
