import type { EngineState } from "../../engine";

export type EngineScreen =
  | "wsl_missing"
  | "install"
  | "stopped"
  | "sleeping"
  | "busy"
  | "error"
  | "running";

/**
 * Which screen shows for an engine state. Only the `running` screen has the
 * resource lists, which are what talks to Docker.
 */
export function screenFor(state: EngineState): EngineScreen {
  switch (state) {
    case "wsl_missing":
      return "wsl_missing";
    case "not_installed":
      return "install";
    case "stopped":
      return "stopped";
    case "sleeping":
      return "sleeping";
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
