// reka-ui pauses every toast on window `blur`, with no setting: switching windows left a toast up
// until coming back. Its listener is added after this one, which stops the event first while a
// toast is open (nothing else in facteur listens for it).
export default defineNuxtPlugin(() => {
  window.addEventListener('blur', (e) => {
    if (document.querySelector('[data-swipe-direction][data-state="open"]')) e.stopImmediatePropagation()
  })
})
