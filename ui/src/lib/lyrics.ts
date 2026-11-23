import type { LyricsLine, LyricsResult } from "@ytmcp/protocol";

export const USER_OFFSET_LIMIT_MS = 10_000;

/**
 * Display time of a line, identical to Rust `effective_line_time_ms`:
 * `timeMs - fileOffsetMs + userOffsetMs`, never below zero. The player clock is untouched.
 * - `fileOffsetMs` is the LRC `[offset:]` tag: positive shows lyrics earlier.
 * - `userOffsetMs` is a delay: positive shows lyrics later, so "earlier" steps it down and "later" up.
 */
export function effectiveLineTimeMs(line: LyricsLine, lyrics: Pick<LyricsResult, "fileOffsetMs" | "userOffsetMs">): number {
  return Math.max(0, line.timeMs - (lyrics.fileOffsetMs ?? 0) + (lyrics.userOffsetMs ?? 0));
}

/** Index of the last synced line whose display time has been reached, or -1. Lines may share a time. */
export function activeLyricIndex(lyrics: LyricsResult | null, positionMs: number): number {
  if (!lyrics || lyrics.kind !== "synced" || !lyrics.syncedLines?.length) return -1;
  let active = -1;
  for (let i = 0; i < lyrics.syncedLines.length; i += 1) {
    if (effectiveLineTimeMs(lyrics.syncedLines[i]!, lyrics) <= positionMs) {
      active = i;
    } else {
      break;
    }
  }
  return active;
}

export function stepUserOffsetMs(currentOffsetMs: number, deltaMs: number): number {
  return Math.max(-USER_OFFSET_LIMIT_MS, Math.min(USER_OFFSET_LIMIT_MS, Math.round(currentOffsetMs + deltaMs)));
}
