import { describe, expect, it } from "vitest";
import { LogBuffer } from "./logBuffer";

function contents(buffer: LogBuffer): string[] {
  return Array.from({ length: buffer.size }, (_, i) => buffer.get(i) as string);
}

describe("LogBuffer", () => {
  it("appends in order and strips line endings", () => {
    const buffer = new LogBuffer(10);
    buffer.append(["a\n", "b\r\n"]);
    buffer.append(["c"]);
    expect(contents(buffer)).toEqual(["a", "b", "c"]);
  });

  it("drops the oldest lines beyond the cap", () => {
    const buffer = new LogBuffer(3);
    buffer.append(["1", "2", "3", "4", "5"]);
    expect(contents(buffer)).toEqual(["3", "4", "5"]);
    expect(buffer.firstIndex).toBe(2);
  });

  it("stays consistent across compaction", () => {
    const buffer = new LogBuffer(4);
    for (let i = 0; i < 100; i += 1) {
      buffer.append([`line ${i}`]);
    }
    expect(contents(buffer)).toEqual([
      "line 96",
      "line 97",
      "line 98",
      "line 99",
    ]);
    expect(buffer.firstIndex).toBe(96);
  });

  it("handles a single batch larger than the cap", () => {
    const buffer = new LogBuffer(2);
    buffer.append(["a", "b", "c", "d"]);
    expect(contents(buffer)).toEqual(["c", "d"]);
  });

  it("clears", () => {
    const buffer = new LogBuffer(2);
    buffer.append(["a", "b", "c"]);
    buffer.clear();
    expect(buffer.size).toBe(0);
    expect(buffer.firstIndex).toBe(0);
    expect(buffer.get(0)).toBeUndefined();
  });
});
