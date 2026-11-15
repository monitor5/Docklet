import { describe, expect, it } from "vitest";
import { FAST_POLL_MS, nextPollDelay, SLOW_POLL_MS } from "./pollDelay";

describe("nextPollDelay", () => {
  it("does not poll while hidden", () => {
    expect(
      nextPollDelay({ state: "starting", visible: false, busy: true }),
    ).toBeNull();
  });

  it("polls slowly in settled states", () => {
    for (const state of [
      "running",
      "stopped",
      "sleeping",
      "not_installed",
    ] as const) {
      expect(nextPollDelay({ state, visible: true, busy: false })).toBe(
        SLOW_POLL_MS,
      );
    }
  });

  it("polls fast in transitions and unsettled states", () => {
    for (const state of [
      "starting",
      "stopping",
      "error",
      "wsl_missing",
      undefined,
    ] as const) {
      expect(nextPollDelay({ state, visible: true, busy: false })).toBe(
        FAST_POLL_MS,
      );
    }
  });

  it("polls fast while a request is in flight", () => {
    expect(nextPollDelay({ state: "running", visible: true, busy: true })).toBe(
      FAST_POLL_MS,
    );
  });
});
