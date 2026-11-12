import { expect, test } from "vitest";
import { ENGINE_STATE_LABEL } from "./engine";

test("every engine state has a non-empty label", () => {
  for (const label of Object.values(ENGINE_STATE_LABEL)) {
    expect(label.length).toBeGreaterThan(0);
  }
});
