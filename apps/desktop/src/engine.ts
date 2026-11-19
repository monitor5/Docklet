/**
 * Mirrors `docklet_core::EngineState` (serde snake_case).
 *
 * `sleeping`: the engine was stopped because nothing used it, and the pipe
 * proxy starts it again for the first Docker client that connects. The
 * window must not be that client by accident, so nothing may call the Docker
 * API in this state; see `usesDockerApi`.
 */
export type EngineState =
  | "wsl_missing"
  | "not_installed"
  | "stopped"
  | "sleeping"
  | "starting"
  | "running"
  | "stopping"
  | "error";

export const ENGINE_STATE_LABEL: Record<EngineState, string> = {
  wsl_missing: "WSL is not installed",
  not_installed: "Not installed",
  stopped: "Stopped",
  sleeping: "Sleeping",
  starting: "Starting",
  running: "Running",
  stopping: "Stopping",
  error: "Error",
};

/**
 * Whether the window may talk to Docker in this state. Only a running
 * engine: a call in any other state fails, and in `sleeping` it would wake
 * an engine nobody asked for.
 */
export function usesDockerApi(state: EngineState): boolean {
  return state === "running";
}

/**
 * Whether the lists reload on this message of the event stream. They do for
 * every Docker event and when the stream is open again (`reconnecting`), but
 * not when it just ended (`disconnected`): nothing can be loaded then, and
 * if the engine went to sleep the reload would wake it. The engine state is
 * looked at again instead, and the next screen follows from that.
 */
export function reloadsLists(kind: string): boolean {
  return kind !== "disconnected";
}
