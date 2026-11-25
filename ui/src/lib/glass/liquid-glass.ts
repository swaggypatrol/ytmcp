/**
 * Liquid glass runtime. Every element with class "lg" that carries `{@attach glass}` gets one
 * SVG filter, used as its backdrop-filter:
 *   field   a displacement map of the surface's rim, assembled from nine pieces: four corner
 *           tiles and four one-texel edge strips (generated once per size of rim and reused),
 *           over a neutral flood. Red and green move the sample inward along the outline's
 *           normal, so the bending follows the rounded corners. Each strip runs on under its
 *           corners (pieceBoxes), so no seam opens where a boundary falls between device pixels.
 *   frost   the backdrop blurred first, evenly over the whole surface: one material, frosted
 *           and refracting at once, with no clearer or cloudier zones
 *   lens    the frost sampled through the field, once per colour channel (dispersion)
 *   mean    a wide blur of the backdrop, refracted the same way, that measures local brightness
 *   tone    a gain derived from that mean, bounding what glyphs sit on
 *   glint   a thin specular line along the outline, from the same field's normal: bright on
 *           the edges facing the light (upper left), a quarter of that on the opposite ones,
 *           and scaled by the local mean so it neither glares over bright covers nor vanishes
 *           over dark ones (rimGainMatrix)
 * Resizing a surface only moves the pieces. Tokens live in glass.css.
 *
 * Without the APIs it needs (or with reduced transparency / forced colours) nothing is built
 * and the CSS fallbacks in glass.css apply.
 */

import { trackFocusSource } from "./focus-source.js";

const SVG_NS = "http://www.w3.org/2000/svg";
/** Rec. 709 luma weights, applied to sRGB-encoded values by the tone stage. */
export const LUMA = [0.2126, 0.7152, 0.0722] as const;
// Direction the light comes from, in screen coordinates (y down): the upper left.
const LIGHT = [-0.6, -0.8] as const;
// Exponent applied to the glint mask: the bend falls as (1 − t)², so this leaves a line about
// the outer tenth of the rim wide.
const GLINT_SHARPNESS = 4;

const KEEP = {
  r: "1 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 0 1",
  g: "0 0 0 0 0  0 1 0 0 0  0 0 0 0 0  0 0 0 0 1",
  b: "0 0 0 0 0  0 0 0 0 0  0 0 1 0 0  0 0 0 0 1",
} as const;

/**
 * feColorMatrix values (4×5, row-major) that turn the local mean into the glint's strength, in
 * alpha only: `overDark` where the mean is black, `overBright` where it is white, linear in its
 * luma between. A filter clamps alpha to 1, so the row is divided by `overDark` and the glint's
 * own amplitude is multiplied by it instead.
 */
export function rimGainMatrix(overDark: number, overBright: number): number[] {
  const slope = (overDark - overBright) / overDark;
  return [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -slope * LUMA[0], -slope * LUMA[1], -slope * LUMA[2], 0, 1];
}

export interface ToneTokens {
  readonly scheme: "dark" | "light";
  /** `--lg-tone-gain`: brightness kept where the backdrop sits at the scheme's own end. */
  readonly gain: number;
  /** `--lg-tone-adapt`: how much is taken away as the local mean moves to the far end. */
  readonly adapt: number;
}

/**
 * feColorMatrix values (4×5, row-major) of the tone stage: R, G and B all become
 * gain = r·R + g·G + b·B + offset of the local mean (sRGB-encoded, clamped to 0…1 by the
 * filter); alpha passes through. Dark glass then outputs `body × gain`, light glass
 * `1 − (1 − body) × gain`. The contrast audit evaluates these same numbers.
 */
export function toneMatrix(tone: ToneTokens): number[] {
  const { gain: a, adapt: b } = tone;
  const row =
    tone.scheme === "light"
      ? [b * LUMA[0], b * LUMA[1], b * LUMA[2], 0, a - b]
      : [-b * LUMA[0], -b * LUMA[1], -b * LUMA[2], 0, a];
  return [...row, ...row, ...row, 0, 0, 0, 0, 1];
}

/**
 * How far the rim bends light, as a fraction of its peak, at depth `t` (0 on the outline, 1 at
 * the inner end of the rim, 0 beyond). The rim's cross-section rises as 1 − (1 − t)³: steep at
 * the outline, flat where it meets the body; for the small angles involved, refraction moves a
 * ray sideways in proportion to the slope, (1 − t)².
 *
 * Depth x then shows what lies at x + reach · (1 − x/rim)². With reach at about 1.4 × the rim
 * (the surfaces' setting) this is a thick lens: the outer part of the rim shows the content
 * just inside it upside down and somewhat compressed, the inner part shows it upright and
 * magnified, and the two meet at a single turning line. That is the rim of real thick glass;
 * the frost blurred in beforehand keeps the turning line soft.
 */
export function refractionProfile(t: number): number {
  if (t <= 0) return 1;
  if (t >= 1) return 0;
  return (1 - t) ** 2;
}

/**
 * Exponent of a corner's curve from its computed CSS `corner-shape`: `superellipse(k)` draws
 * |x|^(2^k) + |y|^(2^k) = 1, so `round` (k = 1) is a circle and `squircle` (k = 2) a quartic.
 * Chromium reports the computed value in the superellipse() form. Anything else (scoops, notches,
 * bevels, no support at all) falls back to a circle.
 */
export function cornerExponent(shape: string): number {
  const value = shape.trim();
  if (value === "squircle") return 4;
  const match = /^superellipse\(\s*([0-9.]+)\s*\)$/.exec(value);
  const k = match ? Number(match[1]) : NaN;
  return k >= 1 && k <= 4 ? 2 ** k : 2;
}

/** One texel of the rim field: the inward offset as a fraction of the reach. */
export interface FieldTexel {
  readonly dx: number;
  readonly dy: number;
}

/**
 * The field at a point (x, y) of the top-left corner region, measured from the surface's
 * top-left, for an outline whose corner radius is `radius`, with a rim `bezel` wide. Other
 * corners and edges are mirrors and slices of this one. Outside the outline and deeper than the
 * rim the texel is neutral.
 */
export function cornerTexel(x: number, y: number, radius: number, bezel: number, exponent: number): FieldTexel {
  const qx = radius - x;
  const qy = radius - y;
  let dist: number; // signed distance-like value: negative inside, by how far
  let nx: number;
  let ny: number;
  if (qx > 0 && qy > 0) {
    const e = exponent;
    const r = (qx ** e + qy ** e) ** (1 / e);
    dist = r - radius;
    const gx = qx ** (e - 1);
    const gy = qy ** (e - 1);
    const len = Math.hypot(gx, gy) || 1;
    nx = -gx / len; // outward normal points left and up
    ny = -gy / len;
  } else if (qx > qy) {
    dist = -x;
    nx = -1;
    ny = 0;
  } else {
    dist = -y;
    nx = 0;
    ny = -1;
  }
  if (dist > 0) return { dx: 0, dy: 0 };
  const p = refractionProfile(-dist / bezel);
  return { dx: -nx * p + 0, dy: -ny * p + 0 }; // + 0 turns −0 into 0
}

interface OpticsTokens extends ToneTokens {
  readonly glint: number;
  readonly glintReturn: number;
  readonly rimOverDark: number;
  readonly rimOverBright: number;
  readonly bezel: number;
  readonly reach: number;
  readonly dispersion: number;
  readonly frost: number;
  readonly meanBlur: number;
  readonly saturation: number;
}

type FilterParts = Record<
  | "neutral" | "frost" | "bendR" | "bendG" | "bendB" | "vivid" | "wide" | "wideBend" | "mean"
  | "gain" | "tone" | "rimGain" | "litMask" | "litShape" | "litShapeA" | "shadeMask" | "shadeShape" | "shadeShapeA"
  | "glint",
  SVGElement
>;

/** The nine pieces of the field, in order: corners TL, TR, BR, BL, then edges T, R, B, L. */
type FieldPieces = readonly SVGFEImageElement[];

interface Surface {
  readonly filter: SVGFilterElement;
  readonly parts: FilterParts;
  readonly pieces: FieldPieces;
  readonly listeners: AbortController;
  optics: OpticsTokens | null;
  /** Key of the rim the pieces currently show, so only a change of rim regenerates them. */
  rimKey: string;
  /** Last border-box size the ResizeObserver reported: fractional and untransformed. */
  box: { width: number; height: number } | null;
}

const surfaces = new Map<HTMLElement, Surface>();
let defs: SVGDefsElement | null = null;
let resizeObserver: ResizeObserver | null = null;
let nextId = 1;
/** While true every surface shows the plain blur: see holdPlainBlur. */
let plainHeld = false;
/** Generated piece images by rim key; a handful of rims exist in the whole app. */
const pieceCache = new Map<string, readonly string[]>();

function supported(): boolean {
  return (
    typeof document !== "undefined" &&
    typeof ResizeObserver !== "undefined" &&
    typeof MutationObserver !== "undefined" &&
    typeof matchMedia === "function" &&
    typeof CSS !== "undefined" &&
    typeof CSS.supports === "function" &&
    CSS.supports("backdrop-filter", "url(#lg)")
  );
}

function opaqueMode(): boolean {
  return (
    matchMedia("(prefers-reduced-transparency: reduce)").matches ||
    matchMedia("(forced-colors: active)").matches ||
    document.documentElement.dataset.reduceTransparency === "true"
  );
}

function svg<T extends SVGElement = SVGElement>(name: string, attrs: Record<string, string | number>): T {
  const node = document.createElementNS(SVG_NS, name) as T;
  set(node, attrs);
  return node;
}
function set(node: Element, attrs: Record<string, string | number>): void {
  for (const key in attrs) node.setAttribute(key, String(attrs[key]));
}

function readTokens(el: HTMLElement): OpticsTokens {
  const cs = getComputedStyle(el);
  const num = (name: string) => parseFloat(cs.getPropertyValue(name)) || 0;
  const rimOverDark = num("--lg-rim-over-dark") || 1;
  return {
    glint: Math.min(1, num("--lg-glint")),
    glintReturn: Math.min(1, num("--lg-glint-return")),
    rimOverDark,
    rimOverBright: num("--lg-rim-over-bright") || rimOverDark,
    bezel: Math.max(2, num("--lg-bezel")),
    reach: num("--lg-reach"),
    dispersion: num("--lg-dispersion"),
    frost: num("--lg-frost"),
    meanBlur: num("--lg-mean-blur"),
    saturation: num("--lg-saturation") || 1,
    gain: num("--lg-tone-gain") || 1,
    adapt: num("--lg-tone-adapt"),
    scheme: cs.getPropertyValue("--lg-scheme").trim() === "light" ? "light" : "dark",
  };
}

/** An axis-aligned box in the surface's own CSS pixels. */
export interface PieceBox {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/** How far each edge strip runs on under its two corner tiles, in CSS px. */
const STRIP_OVERLAP = 1;

/**
 * Where the eight field pieces go on a `width` × `height` surface whose corner tiles are `size`
 * square, in piece order (corners TL, TR, BR, BL, then edges T, R, B, L). A strip runs
 * STRIP_OVERLAP past each end, under the corner tiles, which are painted after it
 * (PIECE_PAINT_ORDER): where a tile's edge falls inside a device pixel (125%, 150%), the tile
 * covers that pixel only in part, and the strip beneath, with the same values there, fills the
 * rest instead of the neutral flood. Without it the rim's glint broke for one device pixel beside
 * every corner.
 */
export function pieceBoxes(width: number, height: number, size: number): readonly PieceBox[] {
  const o = Math.min(STRIP_OVERLAP, size);
  const midW = Math.max(0, width - size * 2) + o * 2;
  const midH = Math.max(0, height - size * 2) + o * 2;
  const box = (x: number, y: number, w: number, h: number): PieceBox => ({ x, y, width: w, height: h });
  return [
    box(0, 0, size, size),
    box(width - size, 0, size, size),
    box(width - size, height - size, size, size),
    box(0, height - size, size, size),
    box(size - o, 0, midW, size),
    box(width - size, size - o, size, midH),
    box(size - o, height - size, midW, size),
    box(0, size - o, size, midH),
  ];
}

/** Paint order of the pieces over the neutral flood: the strips first, the corner tiles on top. */
export const PIECE_PAINT_ORDER = [4, 5, 6, 7, 0, 1, 2, 3] as const;

/** Byte of a signed unit offset: 127.5 ± 127.5 (feDisplacementMap reads C/255 − 0.5). */
const offsetByte = (v: number) => Math.round(127.5 + 127.5 * Math.max(-1, Math.min(1, v)));

/**
 * The eight piece images (PNG data URLs) for a rim: corner tiles `size` × `size` texels and
 * edge strips one texel thick, each texel encoding (dx, dy) as (R, G), blue neutral, alpha opaque.
 */
function renderPieces(size: number, radius: number, bezel: number, exponent: number): readonly string[] {
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d")!;
  const draw = (w: number, h: number, texel: (x: number, y: number) => FieldTexel) => {
    canvas.width = w;
    canvas.height = h;
    const img = ctx.createImageData(w, h);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const f = texel(x + 0.5, y + 0.5);
        const i = (y * w + x) * 4;
        img.data[i] = offsetByte(f.dx);
        img.data[i + 1] = offsetByte(f.dy);
        img.data[i + 2] = 128;
        img.data[i + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);
    return canvas.toDataURL("image/png");
  };
  const corner = (x: number, y: number) => cornerTexel(x, y, radius, bezel, exponent);
  // Mirror the top-left corner into the others; flipping an axis flips that offset's sign.
  const flip = (fx: boolean, fy: boolean) => (x: number, y: number) => {
    const f = corner(fx ? size - x : x, fy ? size - y : y);
    return { dx: fx ? -f.dx : f.dx, dy: fy ? -f.dy : f.dy };
  };
  // Edge strips: the corner's profile along one axis, far from the curve.
  const edge = (vertical: boolean, far: boolean) => (x: number, y: number) => {
    const d = vertical ? x : y;
    const f = corner(vertical ? (far ? size - d : d) : size * 4, vertical ? size * 4 : far ? size - d : d);
    return vertical ? { dx: far ? -f.dx : f.dx, dy: 0 } : { dx: 0, dy: far ? -f.dy : f.dy };
  };
  return [
    draw(size, size, flip(false, false)),
    draw(size, size, flip(true, false)),
    draw(size, size, flip(true, true)),
    draw(size, size, flip(false, true)),
    draw(1, size, edge(false, false)),
    draw(size, 1, edge(true, true)),
    draw(1, size, edge(false, true)),
    draw(size, 1, edge(true, false)),
  ];
}

function createFilter(id: string): Pick<Surface, "filter" | "parts" | "pieces"> {
  const filter = svg<SVGFilterElement>("filter", {
    id,
    filterUnits: "userSpaceOnUse",
    primitiveUnits: "userSpaceOnUse",
    // The default linearRGB would decode the displacement field and wreck it.
    "color-interpolation-filters": "sRGB",
  });
  const bend = (input: string, result: string) =>
    svg("feDisplacementMap", { in: input, in2: "field", xChannelSelector: "R", yChannelSelector: "G", result });
  const matrix = (input: string, values: string, result: string) =>
    svg("feColorMatrix", { in: input, type: "matrix", values, result });
  const blend = (a: string, b: string, result: string) => svg("feBlend", { in: a, in2: b, mode: "screen", result });

  const pieces = Array.from({ length: 8 }, (_, i) =>
    svg<SVGFEImageElement>("feImage", { preserveAspectRatio: "none", result: `piece${i}` })
  );
  const parts: FilterParts = {
    neutral: svg("feFlood", { "flood-color": "rgb(128,128,255)", result: "neutral" }),
    frost: svg("feGaussianBlur", { in: "SourceGraphic", result: "frost" }),
    bendR: bend("frost", "bentR"),
    bendG: bend("frost", "bentG"),
    bendB: bend("frost", "bentB"),
    vivid: svg("feColorMatrix", { in: "lens", type: "saturate", result: "vivid" }),
    wide: svg("feGaussianBlur", { in: "SourceGraphic", result: "wide" }),
    wideBend: bend("wide", "wideLens"),
    mean: svg("feColorMatrix", { in: "wideLens", type: "saturate", result: "mean" }),
    gain: svg("feColorMatrix", { in: "mean", type: "matrix", result: "gain" }),
    tone: svg("feComposite", { in: "vivid", in2: "gain", operator: "arithmetic", k1: 1, k2: 0, result: "toned" }),
    rimGain: svg("feColorMatrix", { in: "mean", type: "matrix", result: "rimGain" }),
    // Glint: alpha = how much the outline faces the light × how strongly it bends there, then
    // sharpened into a thin line. The field's offset points inward, so the outward normal is its
    // negative; the offset's length is already the bending profile.
    litMask: matrix("field", glintRow(1), "litRaw"),
    litShape: svg("feComponentTransfer", { in: "litRaw", result: "lit" }),
    litShapeA: svg("feFuncA", { type: "gamma", offset: 0 }),
    shadeMask: matrix("field", glintRow(-1), "shadeRaw"),
    shadeShape: svg("feComponentTransfer", { in: "shadeRaw", result: "shade" }),
    shadeShapeA: svg("feFuncA", { type: "gamma", offset: 0 }),
    glint: svg("feFlood", { "flood-color": "#fff", result: "white" }),
  };
  const field = svg("feMerge", { result: "field" });
  for (const name of ["neutral", ...PIECE_PAINT_ORDER.map((i) => `piece${i}`)]) field.appendChild(svg("feMergeNode", { in: name }));
  filter.append(
    // field: neutral flood, then the strips, then the corner tiles over them
    parts.neutral,
    ...pieces,
    field,
    // frost → lens, one refraction per colour channel
    parts.frost,
    parts.bendR,
    parts.bendG,
    parts.bendB,
    matrix("bentR", KEEP.r, "onlyR"),
    matrix("bentG", KEEP.g, "onlyG"),
    matrix("bentB", KEEP.b, "onlyB"),
    blend("onlyR", "onlyG", "joinRG"),
    blend("joinRG", "onlyB", "lens"),
    parts.vivid,
    // the local mean brightness, refracted the same way so it lines up with the lens
    parts.wide,
    parts.wideBend,
    parts.mean,
    // tone: lens × gain(mean) for dark glass, 1 − (1 − lens) × gain(mean) for light glass
    parts.gain,
    parts.tone,
    // glint on top of everything, scaled by the local mean
    parts.litMask,
    parts.litShape,
    parts.shadeMask,
    parts.shadeShape,
    svg("feComposite", { in: "lit", in2: "shade", operator: "arithmetic", k2: 1, k3: 1, result: "glintRaw" }),
    parts.rimGain,
    svg("feComposite", { in: "glintRaw", in2: "rimGain", operator: "in", result: "glintMask" }),
    parts.glint,
    svg("feComposite", { in: "white", in2: "glintMask", operator: "in", result: "glintLight" }),
    svg("feComposite", { in: "glintLight", in2: "toned", operator: "over" })
  );
  parts.litShape.appendChild(parts.litShapeA);
  parts.shadeShape.appendChild(parts.shadeShapeA);
  defs!.appendChild(filter);
  return { filter, parts, pieces };
}

/**
 * feColorMatrix that writes into alpha `side` × (outward normal · light) × bend, read from the
 * field's red and green bytes (offset = 2·C − 1 inward), clamped to 0…1 by the filter.
 */
function glintRow(side: 1 | -1): string {
  // outward·L = −(offset·L) = −((2R − 1)·Lx + (2G − 1)·Ly)
  const r = -2 * LIGHT[0] * side;
  const g = -2 * LIGHT[1] * side;
  const k = (LIGHT[0] + LIGHT[1]) * side;
  return `0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  ${r.toFixed(4)} ${g.toFixed(4)} 0 0 ${k.toFixed(4)}`;
}

/** Applies the current CSS frost, tint, and rim tracks to the overlay's own optical stages. */
function syncOverlayOptics(el: HTMLElement, surface: Surface, style = getComputedStyle(el)): void {
  const tokens = surface.optics;
  if (!tokens || !el.hasAttribute("data-lg-lens")) return;
  const { frost, gain, glint, vivid } = surface.parts;
  const track = (name: string) => Number(style.getPropertyValue(`--lg-morph-${name}`));
  const setIfChanged = (node: Element, name: string, value: string) => {
    if (node.getAttribute(name) !== value) node.setAttribute(name, value);
  };
  const tint = track("tint");
  setIfChanged(vivid, "values", (1 + (tokens.saturation - 1) * tint).toFixed(3));
  setIfChanged(frost, "stdDeviation", (tokens.frost * track("frost")).toFixed(3));
  setIfChanged(gain, "values", toneMatrix({
    scheme: tokens.scheme, gain: 1 + (tokens.gain - 1) * tint, adapt: tokens.adapt * tint,
  }).map((v) => +v.toFixed(4)).join(" "));
  setIfChanged(glint, "flood-opacity", track("rim").toFixed(3));
}

/** Samples optical CSS tracks only while the overlay is entering or leaving. */
function trackOverlayOptics(el: HTMLElement, surface: Surface): void {
  const { signal } = surface.listeners;
  const tracks = new Set<string>();
  let frame = 0;
  const paint = () => {
    syncOverlayOptics(el, surface);
    frame = tracks.size ? requestAnimationFrame(paint) : 0;
  };
  const onTransition = (event: TransitionEvent) => {
    if (event.target !== el || event.pseudoElement || !event.propertyName.startsWith("--lg-morph-")) return;
    if (event.type === "transitionrun") tracks.add(event.propertyName);
    else tracks.delete(event.propertyName);
    if (!frame) frame = requestAnimationFrame(paint);
  };
  for (const type of ["transitionrun", "transitionend", "transitioncancel"] as const) {
    el.addEventListener(type, onTransition, { signal });
  }
  signal.addEventListener("abort", () => cancelAnimationFrame(frame), { once: true });
}

/** Writes the surface's current size and tokens into its filter. */
function update(el: HTMLElement, surface: Surface): void {
  if (plainHeld || opaqueMode()) {
    el.style.removeProperty("--lg-backdrop");
    delete el.dataset.lgLens;
    return;
  }
  // Fractional sizes keep the right and bottom pieces on the outline at 125% and 150%.
  const width = surface.box?.width ?? el.offsetWidth;
  const height = surface.box?.height ?? el.offsetHeight;
  if (width < 2 || height < 2) return;
  const tokens = readTokens(el);
  surface.optics = tokens;
  const p = surface.parts;
  const cs = getComputedStyle(el);
  const radius = Math.min(parseFloat(cs.borderTopLeftRadius) || 0, width / 2, height / 2);
  const exponent = cornerExponent(cs.getPropertyValue("corner-shape"));
  const bezel = Math.min(tokens.bezel, width / 2, height / 2);
  // A corner tile covers the curve and the full rim beside it; edges carry on from there.
  const size = Math.max(1, Math.ceil(Math.min(Math.max(radius, bezel) + 1, width / 2, height / 2)));

  const rimKey = [size, radius.toFixed(1), bezel.toFixed(1), exponent].join(":");
  if (surface.rimKey !== rimKey) {
    let urls = pieceCache.get(rimKey);
    if (!urls) {
      urls = renderPieces(size, radius, bezel, exponent);
      pieceCache.set(rimKey, urls);
    }
    surface.pieces.forEach((piece, i) => piece.setAttribute("href", urls[i]!));
    surface.rimKey = rimKey;
  }
  const boxes = pieceBoxes(width, height, size);
  surface.pieces.forEach((piece, i) => {
    const { x, y, width: w, height: h } = boxes[i]!;
    set(piece, { x, y, width: w, height: h });
  });

  const margin = Math.ceil(Math.max(tokens.frost, tokens.meanBlur) * 3) + 2;
  set(surface.filter, { x: -margin, y: -margin, width: width + margin * 2, height: height + margin * 2 });

  // feDisplacementMap moves by scale × (C/255 − 0.5): a scale of 2 × reach reaches ±reach. A rim
  // narrowed by a small surface keeps its proportions.
  const span = tokens.reach * (bezel / tokens.bezel) * 2;
  set(p.frost, { stdDeviation: tokens.frost });
  set(p.bendR, { scale: (span * (1 + tokens.dispersion)).toFixed(2) });
  set(p.bendG, { scale: span.toFixed(2) });
  set(p.bendB, { scale: (span * (1 - tokens.dispersion)).toFixed(2) });
  set(p.vivid, { values: tokens.saturation });
  set(p.wide, { stdDeviation: tokens.meanBlur });
  set(p.wideBend, { scale: span.toFixed(2) });
  set(p.mean, { values: tokens.saturation });

  // A high exponent keeps only where the bend is near its peak: a line a few px thin. The
  // amplitudes carry the over-dark factor that rimGainMatrix divides out.
  const peak = Math.min(1, tokens.glint * tokens.rimOverDark);
  set(p.litShapeA, { amplitude: peak.toFixed(3), exponent: GLINT_SHARPNESS });
  set(p.shadeShapeA, { amplitude: (peak * tokens.glintReturn).toFixed(3), exponent: GLINT_SHARPNESS });
  set(p.rimGain, { values: rimGainMatrix(tokens.rimOverDark, tokens.rimOverBright).map((v) => +v.toFixed(4)).join(" ") });

  set(p.gain, { values: toneMatrix(tokens).map((v) => +v.toFixed(4)).join(" ") });
  set(p.tone, tokens.scheme === "light" ? { k3: -1, k4: 1 } : { k3: 0, k4: 0 });

  // data-lg-lens tells the stylesheet the tone-mapped lens is active, so the light tint is safe.
  el.style.setProperty("--lg-backdrop", `url(#${surface.filter.id})`);
  el.dataset.lgLens = "";
  if (el.classList.contains("lg-overlay")) syncOverlayOptics(el, surface, cs);
}

/** Re-reads tokens for every surface (theme, transparency setting, UI scale, pixel ratio). */
function refreshGlass(): void {
  for (const [el, surface] of surfaces) update(el, surface);
}

/**
 * Holds every surface on the plain blur (glass.css's fallback) while `held`, and rebuilds the
 * lenses at their current size, in the same frame, once released. Used while the sidebar glides
 * (SidebarMotion): the sliding clips change what each surface shows on every frame, and a lens
 * kept in step dropped frames (the width animation before the glide missed a vsync on 16.4% of
 * them; Measurements in docs/design/liquid-glass.md), while the plain blur follows for free.
 */
export function holdPlainBlur(held: boolean): void {
  if (plainHeld === held) return;
  plainHeld = held;
  refreshGlass();
}

// Follow the device pixel ratio across monitors (UI scale changes it too).
function watchPixelRatio(): void {
  matchMedia(`(resolution: ${devicePixelRatio}dppx)`).addEventListener(
    "change",
    () => {
      refreshGlass();
      watchPixelRatio();
    },
    { once: true }
  );
}

/** One-time document plumbing: the filter holder and the observers every surface shares. */
function start(): void {
  const holder = svg("svg", { width: 0, height: 0, "aria-hidden": "true", style: "position:absolute" });
  defs = svg<SVGDefsElement>("defs", {});
  holder.appendChild(defs);
  document.body.appendChild(holder);

  resizeObserver = new ResizeObserver((entries) => {
    const touched = new Set<HTMLElement>();
    for (const entry of entries) {
      const el = entry.target.closest<HTMLElement>(".lg");
      const surface = el ? surfaces.get(el) : undefined;
      if (!el || !surface) continue;
      const size = entry.target === el ? entry.borderBoxSize?.[0] : undefined;
      if (size) surface.box = { width: size.inlineSize, height: size.blockSize };
      touched.add(el);
    }
    for (const el of touched) update(el, surfaces.get(el)!);
  });
  watchPixelRatio();
  matchMedia("(prefers-reduced-transparency: reduce)").addEventListener("change", refreshGlass);
  matchMedia("(forced-colors: active)").addEventListener("change", refreshGlass);
  new MutationObserver(refreshGlass).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme", "data-reduce-transparency", "style"],
  });
}

/** Furthest the pointer moves the rim light sideways, either way, in % of the surface width. */
const LIGHT_SWING = 8;

/**
 * The rim light's sideways shift for a pointer at `fraction` (0 left edge, 1 right edge) of the
 * surface: a small nudge around the resting upper-left position, never a second light source.
 */
export function lightShift(fraction: number): string {
  const f = Math.min(1, Math.max(0, fraction));
  return `${((f - 0.5) * 2 * LIGHT_SWING).toFixed(1)}%`;
}

// Pointer position nudges the specular highlight. One rAF per pointer burst; nothing runs
// while the pointer is still, the page is hidden or motion is reduced.
function trackLight(el: HTMLElement, signal: AbortSignal): void {
  let pending = false;
  let lastX = 0;
  const motionReduced = () =>
    matchMedia("(prefers-reduced-motion: reduce)").matches || document.documentElement.dataset.reduceMotion === "true";
  el.addEventListener(
    "pointermove",
    (event) => {
      lastX = event.clientX;
      if (pending || document.hidden || motionReduced()) return;
      pending = true;
      requestAnimationFrame(() => {
        pending = false;
        const r = el.getBoundingClientRect();
        if (r.width > 0) el.style.setProperty("--lg-light-shift", lightShift((lastX - r.left) / r.width));
      });
    },
    { signal, passive: true }
  );
  el.addEventListener("pointerleave", () => el.style.removeProperty("--lg-light-shift"), { signal });
}

/**
 * Svelte attachment for a glass surface: `<div class="lg lg--bar" {@attach glass}>`.
 */
export function glass(el: HTMLElement): (() => void) | void {
  // The sidebar's glass is always mounted, so this is where the focus ring's source tracking
  // starts, whether or not the lens can be built.
  trackFocusSource();
  if (!supported() || surfaces.has(el)) return;
  if (!defs) start();
  const listeners = new AbortController();
  const newSurface = { ...createFilter(`lg-lens-${nextId++}`), listeners, rimKey: "", box: null, optics: null };
  surfaces.set(el, newSurface);
  if (el.classList.contains("lg-overlay")) trackOverlayOptics(el, newSurface);
  resizeObserver!.observe(el);
  trackLight(el, listeners.signal);

  return () => {
    const surface = surfaces.get(el);
    if (!surface) return;
    const release = () => {
      resizeObserver!.unobserve(el);
      surface.listeners.abort();
      surface.filter.remove();
      el.style.removeProperty("--lg-backdrop");
      el.style.removeProperty("--lg-light-shift");
      delete el.dataset.lgLens;
      surfaces.delete(el);
    };
    // Svelte removes a component's nodes before its attachments clean up, so a surface still in
    // the document is one its owner keeps on screen (a dialog finishing its exit after its page
    // went): it keeps its lens until it leaves the document.
    if (!el.isConnected) return release();
    const watch = new MutationObserver(() => {
      if (el.isConnected) return;
      watch.disconnect();
      release();
    });
    watch.observe(document.body, { childList: true, subtree: true });
  };
}
