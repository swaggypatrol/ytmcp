import type { AudioQualityLevel } from "@ytmcp/protocol";

/**
 * The quality of the playing audio stream as the official page itself labels it (its LOW, MEDIUM
 * and HIGH), passed through by the bridge. Nothing here guesses a tier from a stream number.
 */
export type AudioQuality = AudioQualityLevel;

const CODEC_FAMILIES: readonly (readonly [prefix: string, name: string])[] = [
  ["mp4a", "AAC"],
  ["opus", "Opus"],
  ["vorbis", "Vorbis"],
  ["flac", "FLAC"],
  ["ec-3", "Dolby Digital Plus"],
  ["ac-3", "Dolby Digital"],
];

/**
 * The codec family in plain words ("mp4a.40.2 (140)" → "AAC", "opus" → "Opus"), or `null`
 * for an unknown codec. The full codec string is for Diagnostics only.
 */
export function codecFamily(codec: string | null | undefined): string | null {
  const id = codec?.trim().toLowerCase() ?? "";
  if (!id) return null;
  return CODEC_FAMILIES.find(([prefix]) => id.startsWith(prefix))?.[1] ?? null;
}
