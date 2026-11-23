import type { BridgeEpoch, QueueCompleteness, QueueEntry, QueueEntryId, QueueSnapshot } from "@ytm-glass/protocol";
import { sanitizeTrack } from "./guard.js";
import type { PageQueue } from "./types.js";

interface MappedEntry {
  readonly entryId: QueueEntryId;
  readonly key: string;
  readonly index: number;
}

function countKeys(keys: readonly string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const k of keys) counts.set(k, (counts.get(k) ?? 0) + 1);
  return counts;
}

/**
 * Mirrors the page queue as a `QueueSnapshot` with occurrence-scoped entry ids
 * (`qe:<bridgeEpoch>:<n>`). An entry keeps its id across snapshots only when its page
 * occurrence key is unique in both the previous and the current window, or when the
 * window is unchanged (same start and key sequence). Otherwise ambiguous or unparsable
 * occurrences get fresh ids rather than a guessed mapping. Duplicate videoIds always
 * receive distinct ids.
 */
export class QueueMapper {
  private readonly bridgeEpoch: BridgeEpoch;
  private counter = 0;
  private revision = 0;
  private signature = "";
  private entries: MappedEntry[] = [];
  private last: QueueSnapshot | null = null;

  public constructor(bridgeEpoch: BridgeEpoch) {
    this.bridgeEpoch = bridgeEpoch;
  }

  public get currentRevision(): number {
    return this.revision;
  }

  /** Absolute page index and occurrence key of a mirrored entry, if it is still present. */
  public locate(entryId: QueueEntryId): { index: number; key: string } | null {
    const e = this.entries.find((m) => m.entryId === entryId);
    return e && e.key !== "?" ? { index: e.index, key: e.key } : null;
  }

  public update(queue: PageQueue | null): { snapshot: QueueSnapshot; changed: boolean } {
    const items = queue?.items ?? [];
    const keys = items.map((i) => i.key);
    const newCounts = countKeys(keys);
    const oldCounts = countKeys(this.entries.map((e) => e.key));
    const oldIdByKey = new Map(this.entries.map((e) => [e.key, e.entryId]));
    // An unchanged window keeps every id by position, including unparsable and ambiguous rows.
    const start = queue?.start ?? 0;
    const sameWindow =
      this.entries.length === items.length && this.entries.every((e, i) => e.index === start + i && e.key === keys[i]);

    const mapped: MappedEntry[] = [];
    const entries: QueueEntry[] = [];
    items.forEach((item, i) => {
      const reusable = item.key !== "?" && newCounts.get(item.key) === 1 && oldCounts.get(item.key) === 1;
      const entryId = sameWindow
        ? this.entries[i]!.entryId
        : (reusable ? oldIdByKey.get(item.key) : undefined) ?? this.allocate();
      const index = start + i;
      mapped.push({ entryId, key: item.key, index });
      const availability = item.available ? "available" : "unavailable";
      const track = sanitizeTrack({ ...item, availability });
      entries.push({ entryId, videoId: track.videoId, track, availability });
    });

    const selected = queue?.selectedIndex ?? null;
    const currentEntryId = selected === null ? null : mapped.find((m) => m.index === selected)?.entryId ?? null;
    const truncated = queue !== null && (queue.start > 0 || queue.start + items.length < queue.total);
    const open = queue !== null && (queue.infinite || queue.hasAutoplayTail);
    let completeness: QueueCompleteness = "unknown";
    let hasMore: boolean | null = null;
    // The explicit list can be fully loaded while an autoplay tail keeps the queue open;
    // appends land before that tail, so its end stays provable.
    let explicitComplete: boolean | null = null;
    // Kept apart from the fields above: official album queues keep a continuation after every
    // entry has loaded, and single edits and appends work on them as on a finite queue.
    let mayLoadMore: boolean | null = null;
    if (queue) {
      completeness = truncated || open ? "partial" : "complete";
      hasMore = queue.start + items.length < queue.total || open;
      explicitComplete = !truncated && !queue.infinite;
      mayLoadMore = queue.continuation;
    }
    const loadedRange = queue && items.length > 0 ? { start: queue.start, endExclusive: queue.start + items.length } : null;

    const signature = JSON.stringify([
      currentEntryId,
      completeness,
      hasMore,
      explicitComplete,
      mayLoadMore,
      loadedRange,
      entries.map((e) => [e.entryId, e.videoId, e.track.title, e.availability, e.track.durationMs ?? null]),
    ]);
    const changed = signature !== this.signature || this.last === null;
    if (changed) {
      this.revision += 1;
      this.signature = signature;
    }
    this.entries = mapped;
    this.last = {
      bridgeEpoch: this.bridgeEpoch,
      queueRevision: this.revision,
      currentEntryId,
      entries,
      loadedRange,
      completeness,
      hasMore,
      explicitComplete,
      mayLoadMore,
    };
    return { snapshot: this.last, changed };
  }

  private allocate(): QueueEntryId {
    this.counter += 1;
    return `qe:${this.bridgeEpoch}:${this.counter}`;
  }
}
