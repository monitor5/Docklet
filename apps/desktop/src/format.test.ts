import { expect, test } from "vitest";
import {
  composeProject,
  containerName,
  formatBytes,
  publishedPorts,
  shortId,
} from "./format";

test("formats byte sizes", () => {
  expect(formatBytes(0)).toBe("0 B");
  expect(formatBytes(999)).toBe("999 B");
  expect(formatBytes(1500)).toBe("1.5 KB");
  expect(formatBytes(412_000_000)).toBe("412 MB");
  expect(formatBytes(2_340_000_000)).toBe("2.3 GB");
});

test("container name drops the leading slash and falls back to the id", () => {
  expect(containerName({ Id: "abcdef0123456789", Names: ["/web"] })).toBe(
    "web",
  );
  expect(containerName({ Id: "abcdef0123456789" })).toBe("abcdef012345");
});

test("published ports are unique, sorted and TCP only", () => {
  expect(
    publishedPorts([
      { IP: "::", PrivatePort: 80, PublicPort: 8080, Type: "tcp" },
      { IP: "0.0.0.0", PrivatePort: 80, PublicPort: 8080, Type: "tcp" },
      { PrivatePort: 443, PublicPort: 443, Type: "tcp" },
      { PrivatePort: 53, PublicPort: 5353, Type: "udp" },
      { PrivatePort: 9000, Type: "tcp" },
    ]),
  ).toEqual([443, 8080]);
  expect(publishedPorts(undefined)).toEqual([]);
});

test("short id strips the digest prefix", () => {
  expect(shortId("sha256:0123456789abcdef0123")).toBe("0123456789ab");
});

test("compose project comes from the label", () => {
  expect(
    composeProject({
      Id: "a",
      Labels: { "com.docker.compose.project": "shop" },
    }),
  ).toBe("shop");
  expect(composeProject({ Id: "a" })).toBeUndefined();
});
