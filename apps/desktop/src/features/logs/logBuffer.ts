/** Lines kept in memory per log view; older lines are dropped. */
export const LOG_BUFFER_CAP = 50_000;

/**
 * A bounded line buffer. Appending is amortised O(batch): dropped lines are
 * skipped with an offset and the backing array is compacted only occasionally.
 */
export class LogBuffer {
  private items: string[] = [];
  private start = 0;
  /** Number of lines dropped from the front so far. */
  private droppedCount = 0;

  constructor(private readonly cap: number = LOG_BUFFER_CAP) {}

  get size(): number {
    return this.items.length - this.start;
  }

  /** Absolute number of the first retained line; stable keys for rendering. */
  get firstIndex(): number {
    return this.droppedCount;
  }

  get(index: number): string | undefined {
    return this.items[this.start + index];
  }

  append(lines: readonly string[]): void {
    for (const line of lines) {
      this.items.push(line.replace(/\r?\n$/, ""));
    }
    const excess = this.size - this.cap;
    if (excess > 0) {
      this.start += excess;
      this.droppedCount += excess;
    }
    if (this.start > this.cap) {
      this.items = this.items.slice(this.start);
      this.start = 0;
    }
  }

  clear(): void {
    this.items = [];
    this.start = 0;
    this.droppedCount = 0;
  }
}
