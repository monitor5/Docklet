import { describe, expect, it } from "vitest";
import { type EngineState, usesDockerApi } from "../../engine";
import { type EngineScreen, screenFor } from "./screen";

const EXPECTED: Record<EngineState, EngineScreen> = {
  wsl_missing: "wsl_missing",
  not_installed: "install",
  stopped: "stopped",
  sleeping: "sleeping",
  starting: "busy",
  stopping: "busy",
  error: "error",
  running: "running",
};

describe("screenFor", () => {
  it("maps every engine state to a screen", () => {
    for (const [state, screen] of Object.entries(EXPECTED)) {
      expect(screenFor(state as EngineState)).toBe(screen);
    }
  });

  it("shows the lists, which call Docker, only when Docker may be called", () => {
    for (const state of Object.keys(EXPECTED) as EngineState[]) {
      expect(screenFor(state) === "running").toBe(usesDockerApi(state));
    }
    // A sleeping engine has a screen of its own, not the stopped one.
    expect(screenFor("sleeping")).not.toBe(screenFor("stopped"));
  });
});
