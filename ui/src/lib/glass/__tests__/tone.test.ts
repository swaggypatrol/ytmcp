// @vitest-environment node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { LUMA, rimGainMatrix, toneMatrix, type ToneTokens } from "../liquid-glass.js";

/**
 * Tone audit: the worst-case contrast of everything written on glass, derived from the token
 * values in glass.css (parsed here, never copied) and the matrix the runtime puts into the
 * filter (`toneMatrix`).
 *
 * Over an even backdrop the frost, the lens and the local mean are all the same colour, so the
 * filter outputs `colour × gain(colour)` (dark) or `1 − (1 − colour) × gain` (light): any colour
 * at all. On top come the CSS tint, the sheen and at most one fill. The audit searches every
 * backdrop colour for the luminance range the result can take and compares each text tone
 * against the nearer end. Textured backdrops are covered by the screenshot sweep.
 */

type Rgba = readonly [number, number, number, number];
type Scheme = "dark" | "light";
type Variant = "sidebar" | "bar" | "popover";

const glassCss = readFileSync(resolve(import.meta.dirname, "../glass.css"), "utf8");
const focusCss = readFileSync(resolve(import.meta.dirname, "../../../styles/motion-a11y.css"), "utf8");

/** Custom properties declared by the top-level rule with exactly this selector. */
function declarations(css: string, selector: string): Record<string, string> {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: Record<string, string> = {};
  let depth = 0;
  let start = 0;
  let found = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "{") {
      if (depth === 0 && text.slice(start, i).trim() === selector) {
        found = true;
        const body = text.slice(i + 1, text.indexOf("}", i));
        for (const match of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) out[match[1]!] = match[2]!.trim();
      }
      depth++;
    } else if (ch === "}") {
      depth--;
      if (depth === 0) start = i + 1;
    } else if (ch === ";" && depth === 0) {
      start = i + 1;
    }
  }
  if (!found) throw new Error(`no rule "${selector}"`);
  return out;
}

/** OKLCH to linear-light sRGB primaries (may leave 0…1 for colours outside the sRGB gamut). */
function oklchToLinear(l: number, c: number, hDeg: number): [number, number, number] {
  const h = (hDeg * Math.PI) / 180;
  const a = c * Math.cos(h);
  const b = c * Math.sin(h);
  const l3 = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m3 = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s3 = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
    -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
    -0.0041960863 * l3 - 0.7034186147 * m3 + 1.707614701 * s3,
  ];
}

/** Resolves a token to linear RGB + alpha. Handles var(), oklch() and color-mix(in oklch, X P%, transparent). */
function parse(tokens: Record<string, string>, name: string): { linear: [number, number, number]; alpha: number } {
  let value = tokens[name];
  if (value === undefined) throw new Error(`token ${name} is not defined`);
  for (let guard = 0; guard < 8 && value.includes("var("); guard++) {
    value = value.replace(/var\((--[\w-]+)\)/g, (_, ref: string) => {
      if (tokens[ref] === undefined) throw new Error(`token ${ref} is not defined`);
      return tokens[ref];
    });
  }
  const mix = /^color-mix\(in oklch,\s*(oklch\([^)]+\))\s+([\d.]+)%,\s*transparent\)$/.exec(value);
  const plain = /^oklch\(([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+))?\)$/.exec(mix ? mix[1]! : value);
  if (!plain) throw new Error(`cannot parse ${name}: ${value}`);
  return {
    linear: oklchToLinear(Number(plain[1]), Number(plain[2]), Number(plain[3])),
    alpha: Number(plain[4] ?? 1) * (mix ? Number(mix[2]) / 100 : 1),
  };
}

/** A surface colour as sRGB-encoded RGBA, the space CSS composites in. Must be inside sRGB. */
function color(tokens: Record<string, string>, name: string): Rgba {
  const { linear, alpha } = parse(tokens, name);
  const encoded = linear.map((v) => {
    if (v < -0.002 || v > 1.002) throw new Error(`${name} is outside sRGB`);
    const x = Math.min(1, Math.max(0, v));
    return x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055;
  });
  return [encoded[0]!, encoded[1]!, encoded[2]!, alpha];
}

/**
 * Relative luminance of an opaque glyph colour as an sRGB display shows it: components outside
 * the gamut are clipped, which only ever moves an ink towards the surface it is read on.
 */
function inkLuminance(tokens: Record<string, string>, name: string): number {
  const { linear, alpha } = parse(tokens, name);
  if (alpha !== 1) throw new Error(`${name} must be opaque`);
  const [r, g, b] = linear.map((v) => Math.min(1, Math.max(0, v)));
  return LUMA[0] * r! + LUMA[1] * g! + LUMA[2] * b!;
}

const toLinear = (v: number) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
const luminance = (c: readonly number[]) => LUMA[0] * toLinear(c[0]!) + LUMA[1] * toLinear(c[1]!) + LUMA[2] * toLinear(c[2]!);
const over = (under: readonly number[], top: Rgba) => under.map((v, k) => v * (1 - top[3]) + top[k]! * top[3]);
const ratio = (a: number, b: number) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);

function tokensFor(scheme: Scheme, variant: Variant): Record<string, string> {
  return {
    ...declarations(glassCss, ":root"),
    ...(scheme === "light" ? declarations(glassCss, ':root[data-theme="light"]') : {}),
    ...declarations(glassCss, `.lg--${variant}`),
  };
}

type Material = "lens" | "plainBlur" | "solid";

/**
 * Luminance range of the surface behind glyphs, over every backdrop colour and fill state;
 * `overlay` is laid over each state (the focus halo).
 */
function glassLuminanceRange(tokens: Record<string, string>, material: Material, overlay?: Rgba): { min: number; max: number } {
  const tone: ToneTokens = {
    scheme: tokens["--lg-scheme"] === "light" ? "light" : "dark",
    gain: Number(tokens["--lg-tone-gain"]),
    adapt: Number(tokens["--lg-tone-adapt"]),
  };
  const matrix = toneMatrix(tone);
  const tint = color(tokens, material === "lens" ? "--lg-tint" : "--lg-tint-plain");
  const sheenColor = color(tokens, "--lg-sheen");
  const sheen: Rgba = [sheenColor[0], sheenColor[1], sheenColor[2], sheenColor[3] * Number(tokens["--lg-sheen-strength"])];
  const fills: Rgba[] = [[0, 0, 0, 0], color(tokens, "--lg-fill-hover"), color(tokens, "--lg-fill-active")];

  let min = Infinity;
  let max = -Infinity;
  const visit = (base: readonly number[], withSheen: boolean) => {
    for (const surface of withSheen ? [base, over(base, sheen)] : [base]) {
      for (const fill of fills) {
        const filled = over(surface, fill);
        const y = luminance(overlay ? over(filled, overlay) : filled);
        min = Math.min(min, y);
        max = Math.max(max, y);
      }
    }
  };
  if (material === "solid") {
    visit(color(tokens, "--lg-solid"), false);
    return { min, max };
  }
  const steps = 24;
  for (let r = 0; r <= steps; r++) for (let g = 0; g <= steps; g++) for (let b = 0; b <= steps; b++) {
    const mean = [r / steps, g / steps, b / steps];
    let body = mean;
    if (material === "lens") {
      // Row R of the 4×5 matrix: coefficients for R, G, B, A, then the offset.
      const gain = Math.min(1, Math.max(0, matrix[0]! * mean[0]! + matrix[1]! * mean[1]! + matrix[2]! * mean[2]! + matrix[4]!));
      body = mean.map((v) => (tone.scheme === "light" ? 1 - (1 - v) * gain : v * gain));
    }
    visit(over(body, tint), true);
  }
  return { min, max };
}

function worstRatio(y: number, range: { min: number; max: number }): number {
  if (y > range.max) return ratio(y, range.max);
  if (y < range.min) return ratio(y, range.min);
  return 1;
}

// The screenshot sweep (tools/probes/glass, `shots`) found the rendered worst case up to 1.2%
// below this model's bound (dark sidebar, primary text on the selected fill: 5.96 against 6.03),
// so the thresholds asserted on the model carry a 1.5% margin.
const MODEL_MARGIN = 1.015;
const BODY_TEXT = ["--lg-text-primary", "--lg-text-secondary", "--lg-status-success", "--lg-status-warning", "--lg-status-danger", "--lg-status-info"];
const MARKS = ["--lg-accent", "--lg-agent", "--lg-icon", "--lg-focus-ring"];

describe("liquid glass tone audit", () => {
  it("builds a well-formed feColorMatrix: three identical gain rows and an untouched alpha row", () => {
    // A malformed values list makes the filter fall back to identity and nothing is dimmed.
    for (const scheme of ["dark", "light"] as const) {
      const m = toneMatrix({ scheme, gain: 0.8, adapt: 0.6 });
      expect(m).toHaveLength(20);
      expect(m.slice(5, 10)).toEqual(m.slice(0, 5));
      expect(m.slice(10, 15)).toEqual(m.slice(0, 5));
      expect(m.slice(15)).toEqual([0, 0, 0, 0, 1]);
      expect(m[3]).toBe(0);
    }
    // Dark glass keeps `gain` over black and gives up `adapt` over white; light glass mirrors it.
    const sum = (m: number[]) => m[0]! + m[1]! + m[2]!;
    const dark = toneMatrix({ scheme: "dark", gain: 0.8, adapt: 0.6 });
    const light = toneMatrix({ scheme: "light", gain: 0.8, adapt: 0.6 });
    expect([dark[4], dark[4]! + sum(dark)]).toEqual([0.8, expect.closeTo(0.2, 6)]);
    expect([light[4]! + sum(light), light[4]]).toEqual([expect.closeTo(0.8, 6), expect.closeTo(0.2, 6)]);
  });

  const cases: [Scheme, Variant, Material][] = [];
  for (const scheme of ["dark", "light"] as const) {
    for (const variant of ["sidebar", "bar", "popover"] as const) {
      for (const material of ["lens", "plainBlur", "solid"] as const) cases.push([scheme, variant, material]);
    }
  }

  it.each(cases)("%s %s (%s): body text ≥ 4.5:1 and marks ≥ 3:1 on any backdrop, with the model margin", (scheme, variant, material) => {
    const tokens = tokensFor(scheme, variant);
    const range = glassLuminanceRange(tokens, material);
    const report = Object.fromEntries([...BODY_TEXT, ...MARKS].map((name) => [name, +worstRatio(inkLuminance(tokens, name), range).toFixed(2)]));
    if (process.env.LG_AUDIT_PRINT) console.log(scheme, variant, material, JSON.stringify(report));
    for (const name of BODY_TEXT) expect(report[name], `${name} on ${scheme} ${variant} ${material}`).toBeGreaterThanOrEqual(4.5 * MODEL_MARGIN);
    for (const name of MARKS) expect(report[name], `${name} on ${scheme} ${variant} ${material}`).toBeGreaterThanOrEqual(3 * MODEL_MARGIN);
  });

  it.each(["dark", "light"] as const)("%s: the prominent button's label reads 4.5:1 on its own fill", (scheme) => {
    const tokens = tokensFor(scheme, "popover");
    const fill = inkLuminance(tokens, "--lg-accent-fill");
    expect(ratio(inkLuminance(tokens, "--lg-on-accent-fill"), fill)).toBeGreaterThanOrEqual(4.5);
  });

  it.each(cases)("%s %s (%s): the focus ring keeps 3:1 over its own halo", (scheme, variant, material) => {
    const tokens = tokensFor(scheme, variant);
    // Every halo the app draws is the ring's own colour at some strength; take the strongest.
    const strengths = [...focusCss.matchAll(/color-mix\(in oklch, var\(--focus-ring\) ([\d.]+)%, transparent\)/g)].map((m) => Number(m[1]) / 100);
    expect(strengths.length).toBeGreaterThan(0);
    const ring = color(tokens, "--lg-focus-ring");
    const halo: Rgba = [ring[0], ring[1], ring[2], Math.max(...strengths)];
    const range = glassLuminanceRange(tokens, material, halo);
    expect(worstRatio(inkLuminance(tokens, "--lg-focus-ring"), range)).toBeGreaterThanOrEqual(3 * MODEL_MARGIN);
  });

  it("maps every generic content token used on glass to the audited palette", () => {
    const remap = declarations(glassCss, ".lg");
    for (const name of ["--text-primary", "--text-secondary", "--text-muted", "--accent", "--status-success", "--status-warning", "--status-danger", "--status-info", "--focus-ring"]) {
      expect(remap[name], name).toMatch(/^var\(--lg-/);
    }
    for (const name of ["--bg-subtle", "--bg-elevated-hover", "--accent-soft", "--control-fill-hover", "--control-fill-active"]) {
      expect(remap[name], name).toMatch(/^var\(--lg-fill-(hover|active)\)$/);
    }
    expect(remap["--bg-elevated"]).toBe("transparent");
  });
});

/** Opacity of a colour token. */
const alphaOf = (tokens: Record<string, string>, name: string) => parse(tokens, name).alpha;

describe("liquid glass edge", () => {
  // The edge tokens at 97a195b, before phase 2. Highlights only ever go down from here.
  const BASELINE = {
    dark: { "--lg-rim-bright": 0.24, "--lg-bezel-light": 0.16, "--lg-sheen": 0.1, "--lg-bezel-shade": 0.28, glint: 0.5 },
    light: { "--lg-rim-bright": 0.6, "--lg-bezel-light": 0.5, "--lg-sheen": 0.3, "--lg-bezel-shade": 0.14, glint: 0.9 },
  } as const;

  it.each(["dark", "light"] as const)("%s: no highlight is brighter than it was at 97a195b", (scheme) => {
    const tokens = tokensFor(scheme, "bar");
    const base = BASELINE[scheme];
    for (const name of ["--lg-rim-bright", "--lg-bezel-light", "--lg-sheen", "--lg-bezel-shade"] as const) {
      expect(alphaOf(tokens, name), name).toBeLessThanOrEqual(base[name] + 1e-9);
    }
    // The lens's glint at its strongest, over a black backdrop.
    expect(Number(tokens["--lg-glint"]) * Number(tokens["--lg-rim-over-dark"])).toBeLessThanOrEqual(base.glint + 1e-9);
  });

  it.each(["dark", "light"] as const)("%s: one light at the upper left; the far side returns at most a quarter", (scheme) => {
    const tokens = tokensFor(scheme, "bar");
    expect(alphaOf(tokens, "--lg-rim-return")).toBeLessThanOrEqual(alphaOf(tokens, "--lg-rim-bright") / 4 + 1e-9);
    expect(Number(tokens["--lg-glint-return"])).toBeLessThanOrEqual(0.25);
    expect(Number(tokens["--lg-glint-return"])).toBeGreaterThan(0);
  });

  it.each(["dark", "light"] as const)("%s: overlays are more solid than the sidebar and the bar", (scheme) => {
    for (const variant of ["sidebar", "bar"] as const) {
      expect(alphaOf(tokensFor(scheme, "popover"), "--lg-tint")).toBeGreaterThan(alphaOf(tokensFor(scheme, variant), "--lg-tint"));
    }
  });

  it("draws hairlines one device pixel wide at 100%, 125%, 150% and 200%", () => {
    expect(declarations(glassCss, ":root")["--lg-px"]).toBe("1px");
    const steps = new Map<number, number>();
    for (const m of glassCss.matchAll(/@media \(min-resolution: ([\d.]+)dppx\) \{ :root \{ --lg-px: ([\d.]+)px; \} \}/g)) {
      steps.set(Number(m[1]), Number(m[2]));
    }
    for (const dppx of [1.25, 1.5, 2]) {
      expect(steps.get(dppx), `${dppx}dppx`).toBeDefined();
      expect(steps.get(dppx)! * dppx).toBeCloseTo(1, 3);
    }
  });

  it("scales the glint from over-dark to over-bright with the local mean, within what a filter can hold", () => {
    const tokens = tokensFor("dark", "bar");
    const overDark = Number(tokens["--lg-rim-over-dark"]);
    const overBright = Number(tokens["--lg-rim-over-bright"]);
    expect(overDark).toBe(1.15);
    expect(overBright).toBe(0.7);
    const m = rimGainMatrix(overDark, overBright);
    expect(m).toHaveLength(20);
    expect(m.slice(0, 15).every((v) => v === 0)).toBe(true);
    const alpha = (rgb: readonly number[]) => m[15]! * rgb[0]! + m[16]! * rgb[1]! + m[17]! * rgb[2]! + m[19]!;
    // The amplitude carries overDark, the row the rest: their product is the strength.
    expect(alpha([0, 0, 0]) * overDark).toBeCloseTo(1.15, 6);
    expect(alpha([1, 1, 1]) * overDark).toBeCloseTo(0.7, 6);
    for (const scheme of ["dark", "light"] as const) {
      const t = tokensFor(scheme, "bar");
      // feFuncA clamps to 1: the peak must fit, or the dark end would be cut off unseen.
      expect(Number(t["--lg-glint"]) * Number(t["--lg-rim-over-dark"])).toBeLessThanOrEqual(1);
    }
  });
});
