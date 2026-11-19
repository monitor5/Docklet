import type { EngineState } from "./engine";

export const FAST_POLL_MS = 4000;
export const SLOW_POLL_MS = 15000;

/**
 * States that only change when somebody acts, which the window hears about.
 * A sleeping engine wakes when some Docker client connects; the slow poll
 * notices, and it only asks `wsl.exe` for its lists, which wakes nothing.
 */
const SETTLED: ReadonlySet<EngineState> = new Set([
  "running",
  "stopped",
  "sleeping",
  "not_installed",
]);

/**
 * How long to wait before the next status poll, or null for no poll at all.
 * Nothing is polled while the document is hidden; the window polls once when
 * it becomes visible again. Fast polling is for transitions and for the time
 * one of the window's own requests is in flight.
 */
export function nextPollDelay(input: {
  state: EngineState | undefined;
  visible: boolean;
  busy: boolean;
}): number | null {
  if (!input.visible) return null;
  if (input.busy || !input.state || !SETTLED.has(input.state)) {
    return FAST_POLL_MS;
  }
  return SLOW_POLL_MS;
}
