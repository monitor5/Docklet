import { useCallback, useState } from "react";

/** Inline, dismissible error state plus a runner that captures failures. */
export function useNotice() {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const run = useCallback(async (key: string, task: () => Promise<unknown>) => {
    setBusy(key);
    setError(null);
    try {
      await task();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  }, []);

  const dismiss = useCallback(() => setError(null), []);
  return { error, setError, dismiss, busy, run };
}
