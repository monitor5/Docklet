import { describe, expect, it } from "vitest";
import { stripAnsi } from "./ansi";

describe("stripAnsi", () => {
  it("removes colour codes", () => {
    expect(stripAnsi("\u001b[32mok\u001b[0m done")).toBe("ok done");
  });

  it("leaves plain text alone", () => {
    expect(stripAnsi("plain [1] text")).toBe("plain [1] text");
  });
});
