// @vitest-environment node
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * Chapter 04 (states and focus) as invariants of the stylesheet: one focus ring for the whole
 * app that keeps 3:1 on the canvas, pressed sinks that reduced motion takes back, selected and
 * hover fills that never stack, and the control sizes the player bar is built from. Glass
 * contrast, including the ring over its halo on glass, is in lib/glass/__tests__/tone.test.ts.
 */

const SRC = resolve(import.meta.dirname, "../../..");
const read = (path: string) => readFileSync(join(SRC, path), "utf8");

interface Rule {
  readonly selector: string;
  readonly decls: Record<string, string>;
  /** Enclosing at-rule preludes, outermost first. */
  readonly at: readonly string[];
}

/** Style rules of a stylesheet, with the @media / @supports blocks they sit in. */
function rules(css: string, at: readonly string[] = []): Rule[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: Rule[] = [];
  let i = 0;
  while (i < text.length) {
    const open = text.indexOf("{", i);
    if (open < 0) break;
    let depth = 1;
    let close = open + 1;
    for (; close < text.length && depth > 0; close++) {
      if (text[close] === "{") depth++;
      else if (text[close] === "}") depth--;
    }
    const prelude = text.slice(i, open).trim().replace(/\s+/g, " ");
    const body = text.slice(open + 1, close - 1);
    if (/^@(media|supports)\b/.test(prelude)) out.push(...rules(body, [...at, prelude]));
    else if (!prelude.startsWith("@")) {
      const decls: Record<string, string> = {};
      for (const part of body.split(";")) {
        const colon = part.indexOf(":");
        if (colon > 0) decls[part.slice(0, colon).trim()] = part.slice(colon + 1).trim().replace(/\s+/g, " ");
      }
      out.push({ selector: prelude, decls, at });
    }
    i = close;
  }
  return out;
}

const cssFiles = [
  ...readdirSync(join(SRC, "styles")).filter((f) => f.endsWith(".css")).map((f) => `styles/${f}`),
  "lib/glass/glass.css",
];
const sheets = Object.fromEntries(cssFiles.map((f) => [f, rules(read(f))]));
const controls = sheets["styles/controls.css"]!;
const focus = sheets["styles/motion-a11y.css"]!;
const forced = (r: Rule) => r.at.some((a) => a.includes("forced-colors"));
const reducedMedia = (r: Rule) => r.at.some((a) => a.includes("prefers-reduced-motion"));
const reducedAttr = (r: Rule) => r.selector.includes('[data-reduce-motion="true"]');

/** The declarations of the rule whose selector is exactly `selector`. */
function decl(sheet: readonly Rule[], selector: string): Record<string, string> {
  const rule = sheet.find((r) => r.selector === selector && r.at.length === 0);
  if (!rule) throw new Error(`no rule "${selector}"`);
  return rule.decls;
}

// ---- colour ----------------------------------------------------------------------------------

type Rgba = readonly [number, number, number, number];

/** sRGB-encoded RGBA of `oklch(L C H / A)`, or of `color-mix(in oklch, X P%, transparent)`. */
function srgb(value: string): Rgba {
  const mix = /^color-mix\(in oklch, (oklch\([^)]+\)) ([\d.]+)%, transparent\)$/.exec(value);
  const m = /^oklch\(([\d.]+) ([\d.]+) ([\d.]+)(?: \/ ([\d.]+))?\)$/.exec(mix ? mix[1]! : value);
  if (!m) throw new Error(`cannot parse ${value}`);
  const [l, c, hDeg] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const h = (hDeg * Math.PI) / 180;
  const a = c * Math.cos(h);
  const b = c * Math.sin(h);
  const l3 = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m3 = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s3 = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const linear = [
    4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
    -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
    -0.0041960863 * l3 - 0.7034186147 * m3 + 1.707614701 * s3,
  ].map((v) => Math.min(1, Math.max(0, v)));
  const enc = linear.map((v) => (v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055));
  return [enc[0]!, enc[1]!, enc[2]!, Number(m[4] ?? 1) * (mix ? Number(mix[2]) / 100 : 1)];
}
const toLinear = (v: number) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
const luminance = (c: readonly number[]) => 0.2126 * toLinear(c[0]!) + 0.7152 * toLinear(c[1]!) + 0.0722 * toLinear(c[2]!);
const over = (under: readonly number[], top: Rgba) => under.map((v, k) => v * (1 - top[3]) + top[k]! * top[3]);
const ratio = (a: number, b: number) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);

/** Palette tokens of the canvas for a theme, with var() references resolved. */
function palette(theme: "dark" | "light"): (name: string) => string {
  const base = rules(read("styles/base.css"));
  const tokens = { ...decl(base, ":root"), ...(theme === "light" ? decl(base, ':root[data-theme="light"]') : {}) };
  const resolveVar = (value: string): string => value.replace(/var\((--[\w-]+)\)/g, (_, name: string) => resolveVar(tokens[name]!));
  return (name) => resolveVar(tokens[name]!);
}

// ---- tests -----------------------------------------------------------------------------------

describe("keyboard focus (S-03)", () => {
  it("draws one ring for the whole app: 1.5 px Ice Melt 2 px outside, over a 5 px halo at 20%", () => {
    const ring = focus.find((r) => r.selector.startsWith(":focus-visible,") && r.at.length === 0)!;
    expect(ring.decls).toMatchObject({ outline: "1.5px solid var(--focus-ring)", "outline-offset": "2px" });
    const halo = focus.find((r) => r.decls["box-shadow"]?.startsWith("0 0 0 5px"))!;
    expect(halo.decls["box-shadow"]).toBe("0 0 0 5px color-mix(in oklch, var(--focus-ring) 20%, transparent)");
    // A glass surface that takes focus keeps its own shadow.
    expect(halo.selector).toMatch(/:not\(\.lg\)$/);
  });

  it("takes the ring back from a text field focused by a click, and only then", () => {
    const quiet = decl(focus, ':root[data-focus-source="pointer"] :is(.search-field:has(input:focus-visible), input:focus-visible, textarea:focus-visible)');
    expect(quiet).toMatchObject({ outline: "none", "box-shadow": "none" });
    // Nothing else depends on the focus source: without it every :focus-visible still rings.
    const uses = Object.values(sheets).flat().filter((r) => r.selector.includes("data-focus-source"));
    expect(uses).toHaveLength(1);
  });

  it("rings row-wide controls on glass inward, so the ring never crosses the glass edge", () => {
    const inset = decl(focus, ".lg :is(.nav-item, .menu-item):focus-visible");
    expect(inset["outline-offset"]).toBe("-1.5px");
    expect(inset["box-shadow"]).toBe("inset 0 0 0 5px color-mix(in oklch, var(--focus-ring) 20%, transparent)");
  });

  it("has no per-control focus styles anywhere else", () => {
    for (const [file, sheet] of Object.entries(sheets)) {
      if (file === "styles/motion-a11y.css") continue;
      for (const r of sheet) {
        if (forced(r)) continue;
        for (const [prop, value] of Object.entries(r.decls)) {
          // Moving the ring (outline-offset) for a clipped row is allowed; drawing one is not.
          const ringish = /^outline(?!-offset)/.test(prop) || (/focus/.test(r.selector) && /box-shadow|border-color/.test(prop));
          expect(ringish && value !== "none" ? `${file}: ${r.selector} { ${prop}: ${value} }` : "").toBe("");
        }
        // The ring colour is only ever drawn by the one rule (tokens may still define it).
        const uses = Object.entries(r.decls).filter(([p, v]) => !p.startsWith("--") && /var\(--(lg-)?focus-ring\)/.test(v));
        expect(uses.map(([p]) => `${file}: ${r.selector} { ${p} }`)).toEqual([]);
      }
    }
    // Nor in components (Tailwind focus/ring utilities or <style> blocks).
    const svelteFiles = readdirSync(join(SRC, "lib/components"))
      .filter((f) => f.endsWith(".svelte"))
      .map((f) => `lib/components/${f}`)
      .concat("app/App.svelte");
    for (const file of svelteFiles) {
      expect(read(file), file).not.toMatch(/\bfocus(-visible|-within)?:(outline|ring)|\boutline-none\b|outline:\s*(?!none)/);
    }
  });

  it.each(["dark", "light"] as const)("%s canvas: the ring keeps 3:1 against every surface it sits on, halo included", (theme) => {
    const p = palette(theme);
    const ring = srgb(p("--focus-ring"));
    const halo: Rgba = [ring[0], ring[1], ring[2], 0.2];
    const pressed = srgb(`color-mix(in oklch, ${p("--text-primary")} 12%, transparent)`);
    for (const name of ["--bg-canvas", "--bg-elevated", "--bg-elevated-hover", "--bg-subtle"]) {
      const surface = srgb(p(name));
      for (const under of [surface, over(surface, pressed)]) {
        const behind = luminance(over(under, halo));
        expect(ratio(luminance(ring), behind), `${theme} ${name}`).toBeGreaterThanOrEqual(3);
      }
    }
  });
});

describe("one reaction for every control (chapter 04)", () => {
  const selectedTokens = [".btn-icon-active", '[aria-current="page"]', '[aria-pressed="true"]', '[aria-checked="true"]'];

  it("never lays the hover fill over a selected control", () => {
    const hovers = controls.filter((r) => r.decls["background-color"] === "var(--control-fill-hover)" && r.selector.includes(":hover"));
    expect(hovers.length).toBeGreaterThan(0);
    for (const r of hovers) {
      // A selector that can match a selected control has to exclude every selected state.
      if (r.selector.startsWith(".toggle") || r.selector.startsWith(".search-field") || r.selector.startsWith(".select")) continue;
      for (const token of selectedTokens) expect(r.selector, token).toContain(token);
      expect(r.selector).toMatch(/:hover:where\(:not\(/);
    }
    const selected = controls.find((r) => r.decls["background-color"] === "var(--control-fill-active)" && selectedTokens.every((t) => r.selector.includes(`${t}`)) && !r.selector.includes(":hover"))!;
    // Selected is the strong fill and an accent glyph; the label keeps the primary ink.
    expect(selected.decls).toMatchObject({ color: "var(--text-primary)", "--icon-ink": "var(--accent)" });
  });

  it("sinks small controls to 0.94, text buttons and chips to 0.96 and row-wide ones to 0.975", () => {
    expect(decl(controls, ".btn-icon, .btn-play-focal, .toggle")["--lg-press-scale"]).toBe("0.94");
    expect(decl(controls, ".btn-primary, .btn-secondary, .btn-ghost, button.chip, a.chip")["--lg-press-scale"]).toBe("0.96");
    expect(decl(controls, ".nav-item, .menu-item")["--lg-press-scale"]).toBe("0.975");
    const press = controls.find((r) => r.decls.transform === "scale(var(--lg-press-scale))")!;
    expect(press.selector).toContain(":active");
    expect(press.decls["transition-duration"]).toBe("var(--lg-duration-quick)");
  });

  it("takes back every pressed movement under reduced motion, by setting and by system preference", () => {
    const moving = Object.entries(sheets).flatMap(([file, sheet]) =>
      sheet
        .filter((r) => !reducedMedia(r) && !reducedAttr(r) && /scale/.test(r.decls.transform ?? ""))
        .filter((r) => /:active|\.is-pressed/.test(r.selector))
        .map((r) => ({ file, sheet, state: r.selector.includes(":active") ? ":active" : ".is-pressed" }))
    );
    expect(moving.length).toBeGreaterThan(0);
    for (const { file, sheet, state } of moving) {
      const undo = (r: Rule) => r.selector.includes(state) && r.decls.transform !== undefined && !/scale/.test(r.decls.transform);
      expect(sheet.some((r) => reducedMedia(r) && undo(r)), `${file} ${state} (media)`).toBe(true);
      expect(sheet.some((r) => reducedAttr(r) && undo(r)), `${file} ${state} (setting)`).toBe(true);
    }
  });

  it("dims disabled controls to 45% and gives them no hover or press", () => {
    const disabled = controls.find((r) => r.selector.endsWith(':is(:disabled, [aria-disabled="true"]:not([aria-busy="true"]))'))!;
    expect(disabled.decls.opacity).toBe("0.45");
    for (const r of controls.filter((r) => /:hover|:active/.test(r.selector) && (r.decls["background-color"] || /scale/.test(r.decls.transform ?? "")) && !r.at.length)) {
      // A field is either a wrapper around its input or the input itself; both forms count.
      if (r.selector.startsWith(".search-field")) expect(r.selector).toBe(".search-field:hover:not(:disabled, :has(input:disabled))");
      else if (/^\.(toggle|select|tile)/.test(r.selector)) expect(r.selector).toContain(":disabled");
      else expect(r.selector, r.selector).toMatch(/:not\([^)]*:disabled, \[aria-disabled="true"\]/);
    }
    expect(decl(controls, ".search-field:is(:disabled, :has(input:disabled))").opacity).toBe("0.45");
  });

  it("leaves a control that only waits for its receipt (aria-busy) undimmed", () => {
    // The heart filling is the confirmation; fading the button out for the round trip would flash.
    const dims = controls.filter((r) => r.decls.opacity === "0.45" && r.selector.includes("aria-disabled"));
    expect(dims.length).toBeGreaterThan(0);
    for (const r of dims) expect(r.selector).toContain('[aria-disabled="true"]:not([aria-busy="true"])');
  });

  it("draws a selected chip inverted, since it has no glyph to turn Rose", () => {
    expect(decl(controls, 'button.chip:is([aria-pressed="true"], [aria-checked="true"])')).toMatchObject({
      "background-color": "var(--text-primary)",
      color: "var(--bg-canvas)",
    });
  });
});

describe("control sizes (S-04)", () => {
  it("gives icon buttons a 40 px hit area and a 20 px glyph, 32 / 16 in list rows", () => {
    expect(decl(controls, ".btn-icon")).toMatchObject({ height: "40px", "min-width": "40px", "border-radius": "9999px" });
    expect(decl(controls, ".btn-icon .icon")).toMatchObject({ width: "20px", height: "20px" });
    expect(decl(controls, ".btn-icon-sm")).toMatchObject({ height: "32px", "min-width": "32px" });
    expect(decl(controls, ".btn-icon-sm .icon")).toMatchObject({ width: "16px", height: "16px" });
    // The old large size stays a valid name.
    expect(decl(controls, ".btn-icon-lg")).toMatchObject({ height: "40px", "min-width": "40px" });
  });

  it("makes the play button a 44 px disc that stands on its own", () => {
    expect(decl(controls, ".btn-play-focal")).toMatchObject({
      width: "44px",
      height: "44px",
      "background-color": "var(--lg-accent-fill)",
      color: "var(--lg-on-accent-fill)",
    });
    expect(decl(controls, ".btn-play-focal .icon")).toMatchObject({ width: "22px", height: "22px" });
  });

  it("gives scrub bars a 20 px hit area while the bar stays 4 px and thickens by transform only", () => {
    const primitives = sheets["styles/primitives.css"]!;
    expect(decl(primitives, ".scrub").height).toBe("20px");
    const track = decl(primitives, ".scrub-track");
    expect(track.height).toBe("4px");
    expect(track.transition).toMatch(/^transform /);
    expect(decl(primitives, ".scrub.is-pressed .scrub-track").transform).toMatch(/scaleY\(/);
  });

  it("gives text buttons on glass the same 40 px hit area", () => {
    expect(decl(controls, ".lg :is(.btn-primary, .btn-secondary, .btn-ghost)")["min-height"]).toBe("40px");
  });
});

describe("page change (01 L3, L4)", () => {
  it("names nothing for a view transition: the page change is scoped to the scroll region, so no glass surface or ancestor is captured", () => {
    // A captured glass surface, or one inside a named ancestor (the toast region), has no backdrop
    // left to refract and paints as an opaque slab or as glass that frosts nothing.
    for (const [file, sheet] of Object.entries(sheets)) {
      const named = sheet.filter((r) => "view-transition-name" in r.decls && r.decls["view-transition-name"] !== "none");
      expect(named.map((r) => `${file}: ${r.selector}`)).toEqual([]);
    }
    const page = sheets["styles/transitions.css"]!.filter((r) => r.selector.startsWith(".app-scroll::view-transition-"));
    expect(page.map((r) => r.decls["animation-duration"])).toEqual(["var(--duration-normal)"]);
  });
});
