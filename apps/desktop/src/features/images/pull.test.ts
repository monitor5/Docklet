import { describe, expect, it } from "vitest";
import { appendBounded, imageLabel } from "./pull";

describe("appendBounded", () => {
  it("keeps the newest lines", () => {
    expect(appendBounded(["a", "b"], "c", 2)).toEqual(["b", "c"]);
    expect(appendBounded([], "a", 2)).toEqual(["a"]);
  });
});

describe("imageLabel", () => {
  it("joins tags", () => {
    expect(
      imageLabel({
        Id: "sha256:abc",
        RepoTags: ["a:1", "a:2"],
        Size: 0,
        Created: 0,
      }),
    ).toBe("a:1, a:2");
  });

  it("falls back to the short id for untagged images", () => {
    expect(
      imageLabel({
        Id: "sha256:0123456789abcdef",
        RepoTags: ["<none>:<none>"],
        Size: 0,
        Created: 0,
      }),
    ).toBe("0123456789ab");
    expect(
      imageLabel({ Id: "sha256:0123456789abcdef", Size: 0, Created: 0 }),
    ).toBe("0123456789ab");
  });
});
