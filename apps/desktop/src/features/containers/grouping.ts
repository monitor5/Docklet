import type { Container } from "../../api";
import { composeProject, containerName } from "../../format";

export type ContainerEntry =
  | { kind: "group"; project: string; containers: Container[] }
  | { kind: "single"; container: Container };

const byName = (a: Container, b: Container) =>
  containerName(a).localeCompare(containerName(b));

/** Compose projects first (by name), then standalone containers (by name). */
export function groupContainers(containers: Container[]): ContainerEntry[] {
  const groups = new Map<string, Container[]>();
  const singles: Container[] = [];
  for (const container of containers) {
    const project = composeProject(container);
    if (!project) {
      singles.push(container);
      continue;
    }
    const members = groups.get(project);
    if (members) members.push(container);
    else groups.set(project, [container]);
  }
  const entries: ContainerEntry[] = [...groups.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([project, members]) => ({
      kind: "group",
      project,
      containers: members.sort(byName),
    }));
  for (const container of singles.sort(byName)) {
    entries.push({ kind: "single", container });
  }
  return entries;
}

export function isRunning(container: Container): boolean {
  return container.State === "running";
}
