import { describe, expect, it } from "vitest";
import type { EngineState } from "../../engine";
import { type EngineScreen, screenFor } from "./screen";

describe("screenFor", () => {
  it("maps every engine state to a screen", () => {
    const expected: Record<EngineState, EngineScreen> = {
      wsl_missing: "wsl_missing",
      not_installed: "install",
      stopped: "stopped",
      starting: "busy",
      stopping: "busy",
      error: "error",
      running: "running",
    };
    for (const [state, screen] of Object.entries(expected)) {
      expect(screenFor(state as EngineState)).toBe(screen);
    }
  });
});
