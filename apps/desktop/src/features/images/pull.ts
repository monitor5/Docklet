import type { Image } from "../../api";
import { shortId } from "../../format";

export const PULL_LINES_CAP = 200;

/** Appends a progress line, keeping only the newest `cap` lines. */
export function appendBounded(
  lines: string[],
  line: string,
  cap: number = PULL_LINES_CAP,
): string[] {
  const next = [...lines, line];
  return next.length > cap ? next.slice(next.length - cap) : next;
}

/** All tags, or the short id for untagged images. */
export function imageLabel(image: Image): string {
  const tags = (image.RepoTags ?? []).filter((tag) => tag !== "<none>:<none>");
  return tags.length > 0 ? tags.join(", ") : shortId(image.Id);
}
