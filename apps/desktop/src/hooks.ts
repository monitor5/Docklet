import { useCallback, useEffect, useRef, useState } from "react";
import { api, type EngineStatus, onDockerEvent, onEngineChanged } from "./api";

const STATUS_POLL_MS = 4000;
const EVENT_DEBOUNCE_MS = 150;

/** Engine state, polled while the window exists and refreshed on demand. */
export function useEngine() {
  const [status, setStatus] = useState<EngineStatus | null>(null);
  // Kept apart: a poll that works again clears its own error, but must not
  // clear what a tray action reported a moment earlier.
  const [pollError, setPollError] = useState<string | null>(null);
  const [trayError, setTrayError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.engineStatus());
      setPollError(null);
    } catch (err) {
      setPollError(String(err));
    }
  }, []);

  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // The next poll is armed only when this one is over, so polls cannot
    // pile up behind a backend that is slow to answer.
    const poll = async () => {
      await refresh();
      if (!stopped) timer = setTimeout(poll, STATUS_POLL_MS);
    };
    poll();
    const unlisten = onEngineChanged((message) => {
      setTrayError(message);
      refresh();
    });
    return () => {
      stopped = true;
      clearTimeout(timer);
      unlisten.then((stop) => stop());
    };
  }, [refresh]);

  const dismissError = useCallback(() => {
    setPollError(null);
    setTrayError(null);
  }, []);

  return { status, error: trayError ?? pollError, dismissError, refresh };
}

/**
 * A Docker list that reloads whenever Docker reports an event, so changes made
 * from any client show up without polling.
 */
export function useDockerList<T>(
  fetcher: () => Promise<T[]>,
  enabled: boolean,
) {
  const [items, setItems] = useState<T[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [watchError, setWatchError] = useState<string | null>(null);
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  const reload = useCallback(async () => {
    try {
      setItems(await fetcherRef.current());
      setError(null);
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    if (!enabled) {
      setItems([]);
      return;
    }
    reload();
    // The backend keeps the subscription alive from here on. If it could not
    // even be asked for, say so: the list would silently stop updating.
    api.watchEvents().then(
      () => setWatchError(null),
      (err) => setWatchError(String(err)),
    );
    let timer: ReturnType<typeof setTimeout> | undefined;
    const unlisten = onDockerEvent(() => {
      clearTimeout(timer);
      timer = setTimeout(reload, EVENT_DEBOUNCE_MS);
    });
    return () => {
      clearTimeout(timer);
      unlisten.then((stop) => stop());
    };
  }, [enabled, reload]);

  return { items, error: error ?? watchError, reload };
}
