import { useCallback, useState } from "react";
import { NO_BUSY_KEYS, withKey, withoutKey } from "./busyKeys";

/**
 * Inline, dismissible error state plus a runner that captures failures.
 * `busy` holds the key of every action that is still running.
 */
export function useNotice() {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(NO_BUSY_KEYS);

  const run = useCallback(async (key: string, task: () => Promise<unknown>) => {
    setBusy((keys) => withKey(keys, key));
    setError(null);
    try {
      await task();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy((keys) => withoutKey(keys, key));
    }
  }, []);

  const dismiss = useCallback(() => setError(null), []);
  return { error, setError, dismiss, busy, run };
}
