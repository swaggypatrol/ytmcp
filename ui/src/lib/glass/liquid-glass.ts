/**
 * Liquid glass runtime. Every element with class "lg" that carries `{@attach glass}` gets one
 * SVG filter, used as its backdrop-filter. The filter has no bitmap inputs:
 *   shape   a flood rectangle the size of the surface
 *   height  the shape blurred across the rim width (a slab with a rounded shoulder)
 *   field   the height's gradient (Sobel): where a ray entering there lands sideways
 *   lens    the backdrop sampled through the field, once per colour channel (dispersion)
 *   plate   a heavily blurred copy shown where the body and the label regions need quiet
 *   tone    a gain derived from the local mean brightness, bounding what glyphs sit on
 * Resizing a surface only rewrites a few numeric attributes. Tokens live in glass.css.
 *
 * Without the APIs it needs (or with reduced transparency / forced colours) nothing is built
 * and the CSS fallbacks in glass.css apply.
 */

const SVG_NS = "http://www.w3.org/2000/svg";
/** Rec. 709 luma weights, applied to sRGB-encoded values by the tone stage. */
export const LUMA = [0.2126, 0.7152, 0.0722] as const;
// exp(-x²/2) falls to ~9 % at x = 2.2: the rim width is 2.2 standard deviations of the shoulder.
const BEZEL_SIGMAS = 2.2;
// Peak slope of a unit step blurred with σ = 1, per pixel.
const UNIT_EDGE_SLOPE = 0.3989;
// Height value where the body plate starts to fade in (0.5 is the outline itself).
const BODY_START = 0.62;

const KEEP = {
  r: "1 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 0 1",
  g: "0 0 0 0 0  0 1 0 0 0  0 0 0 0 0  0 0 0 0 1",
  b: "0 0 0 0 0  0 0 0 0 0  0 0 1 0 0  0 0 0 0 1",
} as const;

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

interface OpticsTokens extends ToneTokens {
  readonly bezel: number;
  readonly reach: number;
  readonly dispersion: number;
  readonly frostClear: number;
  readonly frostPlate: number;
  readonly bodyFrost: number;
  readonly plateFeather: number;
  readonly saturation: number;
}

type FilterParts = Record<
  | "shape" | "shoulder" | "slopeX" | "slopeY" | "soften" | "bendR" | "bendG" | "bendB"
  | "vivid" | "mean" | "bodyMask" | "labels" | "labelMask" | "gain" | "tone",
  SVGElement
>;

interface Surface {
  readonly filter: SVGFilterElement;
  readonly parts: FilterParts;
  readonly labelFloods: SVGElement[];
  readonly listeners: AbortController;
}

const surfaces = new Map<HTMLElement, Surface>();
let defs: SVGDefsElement | null = null;
let resizeObserver: ResizeObserver | null = null;
let nextId = 1;

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
  return {
    bezel: Math.max(2, num("--lg-bezel")),
    reach: num("--lg-reach"),
    dispersion: num("--lg-dispersion"),
    frostClear: num("--lg-frost-clear"),
    frostPlate: num("--lg-frost-plate"),
    bodyFrost: Math.min(1, num("--lg-body-frost")),
    plateFeather: Math.max(1, num("--lg-plate-feather")),
    saturation: num("--lg-saturation") || 1,
    gain: num("--lg-tone-gain") || 1,
    adapt: num("--lg-tone-adapt"),
    scheme: cs.getPropertyValue("--lg-scheme").trim() === "light" ? "light" : "dark",
  };
}

function createFilter(id: string): Pick<Surface, "filter" | "parts" | "labelFloods"> {
  const filter = svg<SVGFilterElement>("filter", {
    id,
    filterUnits: "userSpaceOnUse",
    primitiveUnits: "userSpaceOnUse",
    // The default linearRGB would decode the displacement field and wreck it.
    "color-interpolation-filters": "sRGB",
  });
  const bend = (result: string) =>
    svg("feDisplacementMap", { in: "soft", in2: "field", xChannelSelector: "R", yChannelSelector: "G", result });
  const sobel = (kernelMatrix: string, result: string) =>
    svg("feConvolveMatrix", { in: "height", order: 3, kernelMatrix, bias: 0.5, preserveAlpha: "true", edgeMode: "duplicate", result });
  const matrix = (input: string, values: string, result: string) =>
    svg("feColorMatrix", { in: input, type: "matrix", values, result });
  const blend = (a: string, b: string, result: string) => svg("feBlend", { in: a, in2: b, mode: "screen", result });

  const parts: FilterParts = {
    shape: svg("feFlood", { x: 0, y: 0, "flood-color": "#fff", result: "shape" }),
    shoulder: svg("feGaussianBlur", { in: "shape", result: "shoulder" }),
    // The kernels are written flipped because feConvolveMatrix is a true convolution.
    slopeX: sobel("1 0 -1  2 0 -2  1 0 -1", "slopeX"),
    slopeY: sobel("1 2 1  0 0 0  -1 -2 -1", "slopeY"),
    soften: svg("feGaussianBlur", { in: "SourceGraphic", result: "soft" }),
    bendR: bend("bentR"),
    bendG: bend("bentG"),
    bendB: bend("bentB"),
    vivid: svg("feColorMatrix", { in: "lens", type: "saturate", result: "vivid" }),
    mean: svg("feGaussianBlur", { in: "vivid", result: "mean" }),
    bodyMask: svg("feColorMatrix", { in: "height", type: "matrix", result: "bodyMask" }),
    labels: svg("feMerge", { result: "labels" }),
    labelMask: svg("feGaussianBlur", { in: "labels", result: "labelMask" }),
    gain: svg("feColorMatrix", { in: "mean", type: "matrix", result: "gain" }),
    tone: svg("feComposite", { in: "body", in2: "gain", operator: "arithmetic", k1: 1, k2: 0 }),
  };
  filter.append(
    // shape → height (opaque grey: 0 outside, 0.5 on the outline, 1 on the flat body)
    parts.shape,
    parts.shoulder,
    matrix("shoulder", "0 0 0 1 0  0 0 0 1 0  0 0 0 1 0  0 0 0 0 1", "height"),
    // height → field (R, G = 0.5 + inward slope along x, y)
    parts.slopeX,
    parts.slopeY,
    matrix("slopeX", KEEP.r, "fieldX"),
    matrix("slopeY", KEEP.g, "fieldY"),
    blend("fieldX", "fieldY", "field"),
    // backdrop → lens, one refraction per colour channel
    parts.soften,
    parts.bendR,
    parts.bendG,
    parts.bendB,
    matrix("bentR", KEEP.r, "onlyR"),
    matrix("bentG", KEEP.g, "onlyG"),
    matrix("bentB", KEEP.b, "onlyB"),
    blend("onlyR", "onlyG", "joinRG"),
    blend("joinRG", "onlyB", "lens"),
    parts.vivid,
    parts.mean,
    // plate mask = body (from the height) ∪ label rectangles
    parts.bodyMask,
    parts.labels,
    parts.labelMask,
    svg("feComposite", { in: "labelMask", in2: "bodyMask", operator: "over", result: "mask" }),
    svg("feComposite", { in: "mean", in2: "mask", operator: "in", result: "plate" }),
    svg("feComposite", { in: "plate", in2: "vivid", operator: "over", result: "body" }),
    // tone: body × gain(mean) for dark glass, 1 − (1 − body) × gain(mean) for light glass
    parts.gain,
    parts.tone
  );
  defs!.appendChild(filter);
  return { filter, parts, labelFloods: [] };
}

/** Rectangles (relative to the surface) of the regions whose glyphs need a full plate. */
function labelRects(el: HTMLElement, feather: number) {
  const origin = el.getBoundingClientRect();
  const pad = feather * 0.4;
  const rects: { x: number; y: number; width: number; height: number }[] = [];
  for (const node of el.querySelectorAll("[data-lg-plate]")) {
    const r = node.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    rects.push({ x: r.left - origin.left - pad, y: r.top - origin.top - pad, width: r.width + pad * 2, height: r.height + pad * 2 });
  }
  return rects;
}

/** Writes the surface's current size, label regions and tokens into its filter. */
function update(el: HTMLElement, surface: Surface): void {
  if (opaqueMode()) {
    el.style.removeProperty("--lg-backdrop");
    delete el.dataset.lgLens;
    return;
  }
  const width = el.offsetWidth;
  const height = el.offsetHeight;
  if (width < 2 || height < 2) return;
  const tokens = readTokens(el);
  const p = surface.parts;

  const sigma = Math.min(tokens.bezel, width / 2, height / 2) / BEZEL_SIGMAS;
  const margin = Math.ceil(Math.max(sigma, tokens.plateFeather / 2) * 3) + 2;
  set(surface.filter, { x: -margin, y: -margin, width: width + margin * 2, height: height + margin * 2 });
  set(p.shape, { width, height });
  set(p.shoulder, { stdDeviation: sigma.toFixed(2) });

  // Sobel sums 8× the slope per device pixel. Normalise so the steepest point of the shoulder
  // (the outline) maps to the end of the field's range, whatever the device pixel ratio.
  const peakSlopePerDevicePx = UNIT_EDGE_SLOPE / (sigma * devicePixelRatio);
  const divisor = ((8 * peakSlopePerDevicePx) / 0.5).toFixed(5);
  set(p.slopeX, { divisor });
  set(p.slopeY, { divisor });

  const span = tokens.reach * 2; // field 0…1 ↔ −reach…+reach px
  set(p.soften, { stdDeviation: tokens.frostClear });
  set(p.bendR, { scale: (span * (1 + tokens.dispersion)).toFixed(2) });
  set(p.bendG, { scale: span.toFixed(2) });
  set(p.bendB, { scale: (span * (1 - tokens.dispersion)).toFixed(2) });
  set(p.vivid, { values: tokens.saturation });
  set(p.mean, { stdDeviation: tokens.frostPlate });

  const slope = tokens.bodyFrost / (1 - BODY_START);
  set(p.bodyMask, { values: `0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  ${slope.toFixed(4)} 0 0 0 ${(-slope * BODY_START).toFixed(4)}` });

  const rects = tokens.bodyFrost < 1 ? labelRects(el, tokens.plateFeather) : [];
  while (surface.labelFloods.length < rects.length) {
    const flood = svg("feFlood", { "flood-color": "#fff", result: `label${surface.labelFloods.length}` });
    surface.filter.insertBefore(flood, p.labels);
    p.labels.appendChild(svg("feMergeNode", { in: flood.getAttribute("result")! }));
    surface.labelFloods.push(flood);
  }
  surface.labelFloods.forEach((flood, i) => {
    const r = rects[i] ?? { x: 0, y: 0, width: 0, height: 0 };
    set(flood, { x: r.x.toFixed(1), y: r.y.toFixed(1), width: r.width.toFixed(1), height: r.height.toFixed(1) });
  });
  set(p.labelMask, { stdDeviation: (tokens.plateFeather / 2).toFixed(2) });

  set(p.gain, { values: toneMatrix(tokens).map((v) => +v.toFixed(4)).join(" ") });
  set(p.tone, tokens.scheme === "light" ? { k3: -1, k4: 1 } : { k3: 0, k4: 0 });

  // data-lg-lens tells the stylesheet the tone-mapped lens is active, so the light tint is safe.
  el.style.setProperty("--lg-backdrop", `url(#${surface.filter.id})`);
  el.dataset.lgLens = "";
}

/** Re-reads tokens for every surface (theme, transparency setting, UI scale, pixel ratio). */
function refreshGlass(): void {
  for (const [el, surface] of surfaces) update(el, surface);
}

// The Sobel normalisation depends on the device pixel ratio; follow it across monitors.
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
      if (el && surfaces.has(el)) touched.add(el);
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

// Pointer position steers the specular highlight. One rAF per pointer burst; nothing runs
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
        if (r.width > 0) el.style.setProperty("--lg-light-x", `${(((lastX - r.left) / r.width) * 100).toFixed(1)}%`);
      });
    },
    { signal, passive: true }
  );
  el.addEventListener("pointerleave", () => el.style.removeProperty("--lg-light-x"), { signal });
}

/**
 * Svelte attachment for a glass surface: `<div class="lg lg--bar" {@attach glass}>`.
 * Children marked `data-lg-plate` at mount time get a full frosted plate behind them.
 */
export function glass(el: HTMLElement): (() => void) | void {
  if (!supported() || surfaces.has(el)) return;
  if (!defs) start();
  const listeners = new AbortController();
  surfaces.set(el, { ...createFilter(`lg-lens-${nextId++}`), listeners });
  const observed = [el, ...el.querySelectorAll("[data-lg-plate]")];
  for (const node of observed) resizeObserver!.observe(node);
  trackLight(el, listeners.signal);

  return () => {
    const surface = surfaces.get(el);
    if (!surface) return;
    for (const node of observed) resizeObserver!.unobserve(node);
    surface.listeners.abort();
    surface.filter.remove();
    el.style.removeProperty("--lg-backdrop");
    el.style.removeProperty("--lg-light-x");
    delete el.dataset.lgLens;
    surfaces.delete(el);
  };
}
