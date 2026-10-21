// Panel widths used to be saved as window shares; read as pixels they would be tiny, so they are
// dropped once and the panels start at their default width.
export default defineNuxtPlugin(() => {
  try {
    if (localStorage.getItem('facteur:panel-unit') === 'px') return
    for (const key of ['facteur-sidebar-nav', 'facteur-panel-threads']) localStorage.removeItem(key)
    localStorage.setItem('facteur:panel-unit', 'px')
  } catch {}
})
