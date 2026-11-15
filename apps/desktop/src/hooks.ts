import { useCallback, useEffect, useRef, useState } from "react";
import { api, type EngineStatus, onDockerEvent, onEngineChanged } from "./api";
import { type EngineState, reloadsLists } from "./engine";
import { nextPollDelay } from "./pollDelay";

const EVENT_DEBOUNCE_MS = 150;

/**
 * Engine state, polled while the document is visible and refreshed on demand.
 * `busy` says one of this window's own requests is in flight.
 */
export function useEngine(busy: boolean) {
  const [status, setStatus] = useState<EngineStatus | null>(null);
  // Kept apart: a poll that works again clears its own error, but must not
  // clear what a tray action reported a moment earlier.
  const [pollError, setPollError] = useState<string | null>(null);
  const [trayError, setTrayError] = useState<string | null>(null);

  const stateRef = useRef<EngineState | undefined>(undefined);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const rearmRef = useRef<() => void>(() => {});

  const refresh = useCallback(async () => {
    try {
      const next = await api.engineStatus();
      stateRef.current = next.state;
      setStatus(next);
      setPollError(null);
    } catch (err) {
      setPollError(String(err));
    }
  }, []);

  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      clearTimeout(timer);
      if (stopped) return;
      const delay = nextPollDelay({
        state: stateRef.current,
        visible: !document.hidden,
        busy: busyRef.current,
      });
      if (delay !== null) timer = setTimeout(run, delay);
    };
    // The next poll is armed only when this one is over, so polls cannot
    // pile up behind a backend that is slow to answer.
    const run = async () => {
      clearTimeout(timer);
      await refresh();
      schedule();
    };
    rearmRef.current = schedule;
    run();
    const onVisibility = () => {
      if (document.hidden) clearTimeout(timer);
      else run();
    };
    document.addEventListener("visibilitychange", onVisibility);
    const unlistenEngine = onEngineChanged((message) => {
      setTrayError(message);
      run();
    });
    // A lost or renewed event stream may have hidden a change of the engine.
    const unlistenDocker = onDockerEvent((kind) => {
      if (kind === "disconnected" || kind === "reconnecting") run();
    });
    return () => {
      stopped = true;
      clearTimeout(timer);
      rearmRef.current = () => {};
      document.removeEventListener("visibilitychange", onVisibility);
      unlistenEngine.then((stop) => stop());
      unlistenDocker.then((stop) => stop());
    };
  }, [refresh]);

  // Starting or finishing a request changes the delay of the pending poll.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `busy` is the trigger
  useEffect(() => rearmRef.current(), [busy]);

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
    const unlisten = onDockerEvent((kind) => {
      // A stream that just ended may mean the engine went to sleep, and a
      // reload would wake it; `useEngine` looks at the state instead.
      if (!reloadsLists(kind)) return;
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
