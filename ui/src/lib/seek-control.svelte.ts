import type { CommandReceipt, TrackInstanceId } from "@ytmcp/protocol";
import {
  commitSeekPreview,
  isTerminalCommandStatus,
  reconcileSeekPreview,
  startSeekPreview,
  type SeekPreview,
} from "./seek-preview.js";

type Track = TrackInstanceId | null | undefined;

/**
 * The seek preview behind every seek bar. One instance is shared by the player bar and Now
 * Playing, so both show the same position while either is dragged. The preview follows the drag,
 * holds the released position until the seek settles, and is dropped when it fails, times out or
 * the track changes. A drag that starts on one track never seeks the next one.
 *
 * Usage: `drag()` on input, `commit()` on change, `release()` when the pointer lets go, and
 * `reconcile()` from an effect whenever the track or the last receipt changes.
 */
export class SeekControl {
  preview = $state<SeekPreview | null>(null);
  #sequence = 0;
  /** The track a drag started on; `undefined` while no drag is in progress. */
  #dragTrack: TrackInstanceId | null | undefined = undefined;

  drag(positionMs: number, durationMs: number | null | undefined, track: Track): void {
    if (this.#dragTrack === undefined) this.#dragTrack = track ?? null;
    this.#sequence += 1;
    this.preview = (track ?? null) === this.#dragTrack ? startSeekPreview(positionMs, durationMs, track) : null;
  }

  async commit(
    positionMs: number,
    durationMs: number | null | undefined,
    track: Track,
    seek: (positionMs: number, track: TrackInstanceId | null) => Promise<CommandReceipt | null> | void
  ): Promise<void> {
    const current = track ?? null;
    const started = this.#dragTrack === undefined ? current : this.#dragTrack;
    this.#dragTrack = undefined;
    const sequence = ++this.#sequence;
    if (current !== started) {
      this.preview = null;
      return;
    }
    const preview = commitSeekPreview(startSeekPreview(positionMs, durationMs, current), Date.now());
    this.preview = preview;
    const receipt = await seek(preview.positionMs, current);
    if (sequence !== this.#sequence || this.preview?.trackInstanceId !== current) return;
    this.preview = !receipt || isTerminalCommandStatus(receipt.status)
      ? null
      : commitSeekPreview(this.preview, Date.now(), receipt.commandId);
  }

  /**
   * The pointer let go. A drag that ended where it began sends no change, so its preview is
   * dropped; this waits a task because the browser fires the change after the pointer release.
   */
  release(): void {
    setTimeout(() => {
      this.#dragTrack = undefined;
      if (this.preview && this.preview.committedAtMs === null) this.preview = null;
    }, 0);
  }

  reconcile(track: Track, lastReceipt: CommandReceipt | null | undefined): void {
    const next = reconcileSeekPreview(this.preview, track, lastReceipt, Date.now());
    if (next !== this.preview) this.preview = next;
  }
}
