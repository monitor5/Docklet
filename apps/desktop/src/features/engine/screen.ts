import type { EngineState } from "../../engine";

export type EngineScreen =
  | "wsl_missing"
  | "install"
  | "stopped"
  | "busy"
  | "error"
  | "running";

/** Which screen shows for an engine state. */
export function screenFor(state: EngineState): EngineScreen {
  switch (state) {
    case "wsl_missing":
      return "wsl_missing";
    case "not_installed":
      return "install";
    case "stopped":
      return "stopped";
    case "starting":
    case "stopping":
      return "busy";
    case "error":
      return "error";
    case "running":
      return "running";
  }
}

export const WSL_INSTALL_COMMAND = "wsl --install --no-distribution";
