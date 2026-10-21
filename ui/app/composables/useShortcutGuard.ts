/** Shortcuts of the view underneath must not act while an overlay is open; Nuxt UI's
 *  defineShortcuts only skips keys typed into inputs. */
export function overlayOpen(): boolean {
  if (typeof document === 'undefined') return false
  return !!document.querySelector('[role="dialog"][data-state="open"], [role="alertdialog"][data-state="open"], [role="menu"][data-state="open"], [data-slot="content"][data-state="open"]')
}

/** AltGr (AZERTY on Windows) is reported as Ctrl+Alt; a lone Ctrl, Alt or Meta is another
 *  shortcut. */
export function isHashKey(e: KeyboardEvent): boolean {
  return e.key === '#' && !e.metaKey && e.ctrlKey === e.altKey
}
