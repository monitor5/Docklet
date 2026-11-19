import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EngineState } from "./engine";

export type EngineStatus = {
  state: EngineState;
  /** Present when `state` is `error`. */
  reason?: string;
  bases: string[];
  /** `.wslconfig` sets `networkingMode=mirrored`. */
  mirrored_networking: boolean;
};

/** What a followed log sends; `ended` is its last message. */
type LogMessage =
  | { kind: "lines"; lines: string[] }
  | { kind: "ended"; error: string | null };

/** Docker API models, with the Docker API's own field names. */
export type Port = {
  IP?: string;
  PrivatePort: number;
  PublicPort?: number;
  Type?: string;
};

export type Container = {
  Id: string;
  Names?: string[];
  Image?: string;
  State?: string;
  Status?: string;
  Ports?: Port[];
  Labels?: Record<string, string>;
};

export type Image = {
  Id: string;
  RepoTags?: string[];
  Size: number;
  Created: number;
};

export type Volume = {
  Name: string;
  Driver: string;
  Mountpoint: string;
};

export type Network = {
  Id?: string;
  Name?: string;
  Driver?: string;
  Scope?: string;
};

export type ContainerAction = "start" | "stop" | "restart" | "remove";

export const api = {
  engineStatus: () => invoke<EngineStatus>("engine_status"),
  engineInstall: (
    base: string | undefined,
    onProgress: (line: string) => void,
  ) => {
    const progress = new Channel<string>();
    progress.onmessage = onProgress;
    return invoke<void>("engine_install", { base, progress });
  },
  engineStart: () => invoke<void>("engine_start"),
  engineStop: () => invoke<void>("engine_stop"),
  watchEvents: () => invoke<void>("events_watch"),

  containers: () => invoke<Container[]>("containers"),
  containerAction: (id: string, action: ContainerAction) =>
    invoke<void>("container_action", { id, action }),

  images: () => invoke<Image[]>("images"),
  imageRemove: (id: string) => invoke<void>("image_remove", { id }),
  imagePull: (name: string, onProgress: (line: string) => void) => {
    const progress = new Channel<string>();
    progress.onmessage = onProgress;
    return invoke<void>("image_pull", { name, progress });
  },

  volumes: () => invoke<Volume[]>("volumes"),
  volumeRemove: (name: string) => invoke<void>("volume_remove", { name }),

  networks: () => invoke<Network[]>("networks"),
  networkRemove: (id: string) => invoke<void>("network_remove", { id }),

  /** Opens `http://localhost:<port>` in the browser; the backend builds the URL. */
  openPort: (port: number) => invoke<void>("open_port", { port }),

  /**
   * Follows a container's logs; resolves to a function that stops it.
   * `onEnd` is called once when the log is over, with the error that ended
   * it or null when it simply ended (the container stopped or was removed).
   */
  logs: async (
    id: string,
    onLines: (lines: string[]) => void,
    onEnd: (error: string | null) => void,
  ) => {
    const output = new Channel<LogMessage>();
    output.onmessage = (message) => {
      if (message.kind === "lines") onLines(message.lines);
      else onEnd(message.error);
    };
    const token = await invoke<number>("logs_start", { id, output });
    return () => invoke<void>("logs_stop", { token });
  },
};

/**
 * Docker reported a change of the given kind, or the event stream ended
 * (`disconnected`) or is being opened again (`reconnecting`).
 */
export function onDockerEvent(
  handler: (kind: string) => void,
): Promise<UnlistenFn> {
  return listen<string>("docker-event", (event) => handler(event.payload));
}

/** The engine was started or stopped from the tray; payload is an error or null. */
export function onEngineChanged(
  handler: (error: string | null) => void,
): Promise<UnlistenFn> {
  return listen<string | null>("engine-changed", (event) =>
    handler(event.payload),
  );
}
