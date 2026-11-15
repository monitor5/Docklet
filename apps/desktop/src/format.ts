import type { Container, Port } from "./api";

export function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

/** `/web` → `web`; falls back to the short id. */
export function containerName(container: Container): string {
  const name = container.Names?.[0]?.replace(/^\//, "");
  return name || container.Id.slice(0, 12);
}

/** Host ports published on TCP, without duplicates (IPv4 and IPv6 bindings). */
export function publishedPorts(ports: Port[] | undefined): number[] {
  const published = (ports ?? [])
    .filter((port) => port.PublicPort && (port.Type ?? "tcp") === "tcp")
    .map((port) => port.PublicPort as number);
  return [...new Set(published)].sort((a, b) => a - b);
}

/**
 * The state of a container without the age in Docker's status text
 * (`Up 13 seconds`, `Exited (1) 2 hours ago`), which would go stale between
 * events. Keeps the parts that only change with an event: the exit code and
 * the health check result.
 */
export function stateLabel(container: Container): string {
  const state = container.State || "unknown";
  const status = container.Status ?? "";
  if (state === "exited" || state === "dead") {
    const code = /^(?:Exited|Dead) \((-?\d+)\)/i.exec(status)?.[1];
    return code === undefined ? state : `${state} (${code})`;
  }
  const health = /\((healthy|unhealthy|health: starting)\)/.exec(status)?.[1];
  return health ? `${state} (${health})` : state;
}

export function shortId(id: string): string {
  return id.replace(/^sha256:/, "").slice(0, 12);
}

/** The Compose project a container belongs to, if any. */
export function composeProject(container: Container): string | undefined {
  return container.Labels?.["com.docker.compose.project"];
}
