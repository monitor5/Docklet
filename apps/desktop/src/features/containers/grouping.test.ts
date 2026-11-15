import { describe, expect, it } from "vitest";
import type { Container } from "../../api";
import { groupContainers } from "./grouping";

const make = (name: string, project?: string): Container => ({
  Id: `id-${name}`,
  Names: [`/${name}`],
  Labels: project ? { "com.docker.compose.project": project } : {},
});

describe("groupContainers", () => {
  it("groups by compose project, projects first, sorted by name", () => {
    const entries = groupContainers([
      make("solo-b"),
      make("web", "shop"),
      make("solo-a"),
      make("db", "shop"),
      make("api", "alpha"),
    ]);
    expect(
      entries.map((e) =>
        e.kind === "group"
          ? `${e.project}:${e.containers.map((c) => c.Names?.[0]).join(",")}`
          : e.container.Names?.[0],
      ),
    ).toEqual(["alpha:/api", "shop:/db,/web", "/solo-a", "/solo-b"]);
  });

  it("returns nothing for no containers", () => {
    expect(groupContainers([])).toEqual([]);
  });
});
