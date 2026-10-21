// Before the first render, to avoid a flash of the default colour.
export default defineNuxtPlugin({
  name: 'facteur-appearance',
  enforce: 'pre',
  setup() {
    useAppearance().restore()
  }
})
