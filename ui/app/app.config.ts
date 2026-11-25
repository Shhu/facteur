export default defineAppConfig({
  ui: {
    colors: {
      primary: 'teal',
      neutral: 'zinc'
    },
    // A panel fills the dashboard, no more: a full screen height would run past its insets
    // (status bar, tab bar, navigation bar) on phones.
    dashboardPanel: {
      slots: {
        root: 'min-h-0'
      }
    }
  }
})
