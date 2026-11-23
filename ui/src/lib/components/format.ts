import type { Track } from "@ytm-glass/protocol";
import { t } from "../i18n/index.svelte.js";

export function formatDurationMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) {
    return t.library.durationUnknown;
  }
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

export function formatArtists(track: Track | null | undefined): string {
  if (!track?.artists || track.artists.length === 0) {
    return t.common.unknownArtist;
  }
  return track.artists.map((a) => a.name).join(" / ");
}


/** Badge for tracks that may not play; null for normally available tracks. */
export function availabilityBadge(track: Pick<Track, "availability">): string | null {
  return track.availability === "available" ? null : t.availability[track.availability];
}

/** Whether a play request for this track is worth sending (unknown is attempted). */
export function isPlayable(track: Pick<Track, "availability">): boolean {
  return track.availability === "available" || track.availability === "unknown";
}

/**
 * Best-effort `lang` hint: kana means Japanese and Hangul means Korean. Han-only text is
 * ambiguous between Chinese and Japanese, so it is tagged with the UI language (zh-CN).
 */
export function detectCjkLangHint(text: string): "ja" | "ko" | "zh-CN" | undefined {
  if (/[\u3040-\u30ff]/.test(text)) return "ja";
  if (/[\uac00-\ud7af\u1100-\u11ff]/.test(text)) return "ko";
  if (/[\u4e00-\u9fff]/.test(text)) return "zh-CN";
  return undefined;
}
