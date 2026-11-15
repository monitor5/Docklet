import { expect, test } from "vitest";
import {
  ENGINE_STATE_LABEL,
  type EngineState,
  reloadsLists,
  usesDockerApi,
} from "./engine";

test("every engine state has a non-empty label", () => {
  for (const label of Object.values(ENGINE_STATE_LABEL)) {
    expect(label.length).toBeGreaterThan(0);
  }
});

test("only a running engine is talked to", () => {
  const states = Object.keys(ENGINE_STATE_LABEL) as EngineState[];
  expect(states.filter(usesDockerApi)).toEqual(["running"]);
  // A call would wake a sleeping engine, so it in particular gets none.
  expect(usesDockerApi("sleeping")).toBe(false);
});

test("lists reload on Docker events and on reconnect, not on disconnect", () => {
  for (const kind of ["container", "image", "volume", "network", ""]) {
    expect(reloadsLists(kind)).toBe(true);
  }
  expect(reloadsLists("reconnecting")).toBe(true);
  expect(reloadsLists("disconnected")).toBe(false);
});
