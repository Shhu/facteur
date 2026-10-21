/** Accounts whose server keeps dropping connections, offered the single-connection mode and not
 *  answered yet (filled by the engine's `one_connection_advised` event and at start). */
export function useConnectionAdvice() {
  return useState<number[]>('connection-advice', () => [])
}
