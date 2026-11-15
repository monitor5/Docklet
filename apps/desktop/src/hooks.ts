import { useCallback, useEffect, useRef, useState } from "react";
import { api, type EngineStatus, onDockerEvent, onEngineChanged } from "./api";

const STATUS_POLL_MS = 4000;
const EVENT_DEBOUNCE_MS = 150;

/** Engine state, polled while the window exists and refreshed on demand. */
export function useEngine() {
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.engineStatus());
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, STATUS_POLL_MS);
    const unlisten = onEngineChanged((message) => {
      setError(message);
      refresh();
    });
    return () => {
      clearInterval(timer);
      unlisten.then((stop) => stop());
    };
  }, [refresh]);

  return { status, error, setError, refresh };
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
    api.watchEvents();
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

  return { items, error, reload };
}
