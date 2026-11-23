import type { CommandReceipt, TrackInstanceId } from "@ytm-glass/protocol";

/** How long a committed seek preview may wait for its receipt before falling back to observed time. */
export const SEEK_PREVIEW_TIMEOUT_MS = 4_000;

/**
 * Local seek-bar preview while dragging and until the seek command settles.
 * It never replaces observed playback state; it is bound to one track instance.
 */
export interface SeekPreview {
  readonly positionMs: number;
  readonly trackInstanceId: TrackInstanceId | null;
  readonly commandId: string | null;
  readonly committedAtMs: number | null;
}

export function startSeekPreview(
  positionMs: number,
  durationMs: number | null | undefined,
  trackInstanceId: TrackInstanceId | null | undefined
): SeekPreview {
  const max = durationMs != null && durationMs > 0 ? durationMs : Number.MAX_SAFE_INTEGER;
  return {
    positionMs: Math.max(0, Math.min(Math.round(positionMs), max)),
    trackInstanceId: trackInstanceId ?? null,
    commandId: null,
    committedAtMs: null,
  };
}

/** Marks the preview as released; the command id may arrive later once the receipt is known. */
export function commitSeekPreview(preview: SeekPreview, nowMs: number, commandId: string | null = null): SeekPreview {
  return { ...preview, commandId: commandId ?? preview.commandId, committedAtMs: preview.committedAtMs ?? nowMs };
}

export function isTerminalCommandStatus(status: CommandReceipt["status"]): boolean {
  return status === "observed" || status === "failed" || status === "indeterminate" || status === "cancelled";
}

/**
 * Drops the preview when the track instance changed, when its seek command reached any
 * terminal state (observed, failed, indeterminate, cancelled), or when it waited too long.
 */
export function reconcileSeekPreview(
  preview: SeekPreview | null,
  currentTrackInstanceId: TrackInstanceId | null | undefined,
  lastReceipt: CommandReceipt | null | undefined,
  nowMs: number
): SeekPreview | null {
  if (!preview) return null;
  if (preview.trackInstanceId !== (currentTrackInstanceId ?? null)) return null;
  if (preview.commandId && lastReceipt?.commandId === preview.commandId && isTerminalCommandStatus(lastReceipt.status)) {
    return null;
  }
  if (preview.committedAtMs !== null && nowMs - preview.committedAtMs >= SEEK_PREVIEW_TIMEOUT_MS) {
    return null;
  }
  return preview;
}
