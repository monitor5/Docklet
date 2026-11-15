import { useEffect, useState } from "react";
import { api } from "../../api";
import { LogBuffer } from "./logBuffer";

/** Upper bound between a batch arriving and the view noticing it. */
const FLUSH_FALLBACK_MS = 250;

/**
 * Follows a container's logs into a bounded buffer. Batches go straight into
 * the buffer; the view is told at most once per frame, so a burst of batches
 * costs one render. The stream is stopped when the component unmounts.
 */
export function useLogStream(id: string) {
  const [buffer] = useState(() => new LogBuffer());
  const [version, setVersion] = useState(0);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let scheduled = false;
    let frame = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const flush = () => {
      cancelAnimationFrame(frame);
      clearTimeout(timer);
      scheduled = false;
      if (!cancelled) setVersion((value) => value + 1);
    };
    const schedule = () => {
      if (scheduled) return;
      scheduled = true;
      // rAF is paused while the window is hidden; the timer keeps state fresh.
      frame = requestAnimationFrame(flush);
      timer = setTimeout(flush, FLUSH_FALLBACK_MS);
    };

    buffer.clear();
    setError(null);
    let stop: (() => Promise<void>) | undefined;
    api
      .logs(id, (lines) => {
        if (cancelled) return;
        buffer.append(lines);
        schedule();
      })
      .then((stopFn) => {
        if (cancelled) stopFn().catch(() => {});
        else stop = stopFn;
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      });

    return () => {
      cancelled = true;
      cancelAnimationFrame(frame);
      clearTimeout(timer);
      stop?.().catch(() => {});
    };
  }, [id, buffer]);

  return { buffer, version, error };
}
