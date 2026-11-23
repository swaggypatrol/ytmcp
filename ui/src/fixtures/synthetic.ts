/** Mock-mode only: scenario presets and synthetic artwork for the explicit mock preview. */
import type { FakeScenario, Track } from "@ytm-glass/protocol";

export interface ScenarioPreset {
  id: FakeScenario;
  label: string;
  description: string;
}

export const FAKE_SCENARIO_PRESETS: readonly ScenarioPreset[] = [
  { id: "normal", label: "Normal playback and library", description: "Signed in, with a 10,000-track synthetic library" },
  { id: "loading", label: "Loading", description: "Initial load and pending responses" },
  { id: "empty", label: "Empty library", description: "Empty lists and no track selected" },
  { id: "offlineCached", label: "Offline, cache only", description: "Network down, cache marked stale" },
  { id: "authExpired", label: "Session expired", description: "Sign-in lapsed; prompts to open the official player" },
  { id: "unsupported", label: "Limited capabilities", description: "playPause, seek and others declared unsupported" },
  { id: "bridgeCrashed", label: "Bridge disconnected", description: "The page bridge drops unexpectedly" },
  { id: "indeterminateNext", label: "Next times out", description: "next returns indeterminate and is not retried" },
];

const SYNTHETIC_PALETTES: readonly [string, string, string][] = [
  ["#d9385a", "#4a1c40", "#161824"],
  ["#2e7cf6", "#1d3557", "#121622"],
  ["#199f77", "#134e4a", "#11191d"],
  ["#d97724", "#5c2d16", "#1a1615"],
  ["#8b5cf6", "#312e81", "#161524"],
  ["#e11d48", "#881337", "#1a1218"],
];

function hashSeed(input: string): number {
  let h = 2166136261;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function escapeXml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

/**
 * Generates a self-contained SVG data-URI cover for mock mode only. Real mode shows the
 * Rust-cached `app://covers/` asset or a neutral placeholder, never a fake cover.
 */
export function createSyntheticCoverDataUri(seed: string, label = "YTM"): string {
  const hash = hashSeed(seed || "ytm-glass");
  const palette = SYNTHETIC_PALETTES[hash % SYNTHETIC_PALETTES.length]!;
  const [c1, c2, c3] = palette;
  const glyph = escapeXml((label.trim().slice(0, 2) || "♪").toUpperCase());

  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 160" width="160" height="160">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">` +
    `<stop offset="0%" stop-color="${c1}"/><stop offset="55%" stop-color="${c2}"/>` +
    `<stop offset="100%" stop-color="${c3}"/></linearGradient></defs>` +
    `<rect width="160" height="160" rx="20" fill="url(#g)"/>` +
    `<circle cx="122" cy="38" r="28" fill="white" fill-opacity="0.12"/>` +
    `<circle cx="36" cy="126" r="42" fill="white" fill-opacity="0.08"/>` +
    `<text x="80" y="92" text-anchor="middle" font-family="sans-serif" font-size="40" font-weight="700" fill="white" fill-opacity="0.92">${glyph}</text>` +
    `</svg>`;

  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

/** Invented names in Chinese, Japanese and Korean script: exercises truncation, font fallback and lang hints. */
export const LONG_CJK_MIXED_SAMPLE_TRACK: Track = {
  videoId: "mock-long-cjk-001",
  title:
    "Long Mixed-Script Title Test — Midnight Glass Symphony in Tokyo 深夜城市玻璃交响曲 日本語テストタイトル 한국어 테스트 제목 (Remastered Live 2026)",
  artists: [
    { id: "art-1", name: "星野 灯 / Akari Hoshino (Synthetic)" },
    { id: "art-2", name: "한빛 (Hanbit, Synthetic)" },
    { id: "art-3", name: "林晨 Chen Lin (Synthetic)" },
  ],
  album: {
    id: "alb-cjk-1",
    browseId: null,
    title: "东亚城市声音档案 · East Asia Glass Sessions",
  },
  durationMs: 248_000,
  cover: null,
  availability: "available",
  versionTag: "remaster",
  isLiked: true,
};
