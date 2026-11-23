export type ColorScheme = "dark" | "light";

export interface TrackAccent {
  readonly accent: string;
  readonly accentSoft: string;
  readonly onAccent: string;
}

const OKLCH_RE = /^oklch\(\s*([0-9]*\.?[0-9]+)(%?)\s+([0-9]*\.?[0-9]+)\s+([0-9]*\.?[0-9]+)(?:deg)?\s*\)$/i;

/** Lightness bands that keep accent text >= 4.5:1 on the canvas and on-accent text >= 4.5:1 on fills. */
const L_BAND: Record<ColorScheme, readonly [number, number]> = {
  dark: [0.76, 0.88],
  light: [0.4, 0.5],
};
/** A cover lends its hue, not its loudness: chroma stays in the range of the theme's pastel inks. */
const MAX_CHROMA = 0.09;
const ON_ACCENT: Record<ColorScheme, string> = {
  dark: "oklch(0.2 0.012 300)",
  light: "oklch(0.99 0.003 91.4)",
};

/**
 * Turns a cover's `accentOklch` into accent tokens that stay readable on the current scheme and
 * sit with the theme's pastels, by clamping lightness and chroma. Returns null for missing or malformed input (use theme tokens).
 */
export function accessibleTrackAccent(raw: string | null | undefined, scheme: ColorScheme): TrackAccent | null {
  if (!raw) return null;
  const m = OKLCH_RE.exec(raw.trim());
  if (!m) return null;
  const rawL = Number.parseFloat(m[1]!) / (m[2] === "%" ? 100 : 1);
  const c = Number.parseFloat(m[3]!);
  const h = Number.parseFloat(m[4]!) % 360;
  if (![rawL, c, h].every(Number.isFinite)) return null;

  const [lo, hi] = L_BAND[scheme];
  const l = Math.max(lo, Math.min(hi, rawL));
  const chroma = Math.min(MAX_CHROMA, Math.max(0, c));
  const base = `${l.toFixed(3)} ${chroma.toFixed(3)} ${h.toFixed(1)}`;
  return {
    accent: `oklch(${base})`,
    accentSoft: `oklch(${base} / 0.16)`,
    onAccent: ON_ACCENT[scheme],
  };
}
