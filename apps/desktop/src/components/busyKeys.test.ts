import { describe, expect, it } from "vitest";
import { NO_BUSY_KEYS, withKey, withoutKey } from "./busyKeys";

describe("busy keys", () => {
  it("keeps concurrent actions apart", () => {
    const pulling = withKey(NO_BUSY_KEYS, "pull");
    const both = withKey(pulling, "rm:abc");
    expect([...both].sort()).toEqual(["pull", "rm:abc"]);

    // The removal finishes first; the pull is still busy.
    const afterRemove = withoutKey(both, "rm:abc");
    expect(afterRemove.has("pull")).toBe(true);
    expect(afterRemove.has("rm:abc")).toBe(false);
    expect(withoutKey(afterRemove, "pull").size).toBe(0);
  });

  it("does not change the set it was given", () => {
    const one = withKey(NO_BUSY_KEYS, "a");
    withKey(one, "b");
    withoutKey(one, "a");
    expect([...one]).toEqual(["a"]);
    expect(NO_BUSY_KEYS.size).toBe(0);
  });

  it("returns the same set when nothing changes", () => {
    const one = withKey(NO_BUSY_KEYS, "a");
    expect(withKey(one, "a")).toBe(one);
    expect(withoutKey(one, "b")).toBe(one);
  });
});
