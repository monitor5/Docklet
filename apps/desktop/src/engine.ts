/** Mirrors `docklet_core::EngineState` (serde snake_case). */
export type EngineState =
  | "wsl_missing"
  | "not_installed"
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "error";

export const ENGINE_STATE_LABEL: Record<EngineState, string> = {
  wsl_missing: "WSL is not installed",
  not_installed: "Not installed",
  stopped: "Stopped",
  starting: "Starting",
  running: "Running",
  stopping: "Stopping",
  error: "Error",
};
