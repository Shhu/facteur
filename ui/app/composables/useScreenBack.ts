/** Closes the overlay when the screen covers a mail list, else goes back in the history, else
 *  (opened directly) to `fallback`. */
export function useScreenBack(fallback = '/') {
  const router = useRouter()
  const { covered, close } = useMailOverlay()
  return () => {
    if (covered.value) close()
    else if (window.history.state?.back) router.back()
    else navigateTo(fallback, { replace: true })
  }
}
