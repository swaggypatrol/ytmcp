// Overlay exit (and entry), measured on the window's own frames. See run-probe.mjs, mode "exit".
//
// Frames are taken from the real window at real speed with webContents.beginFrameSubscription.
// It delivers about 30 of the display's 120 frames a second, so every case runs several times
// and the frames of all runs are merged on one timeline (stroboscopic sampling). A small marker
// in the window's corner carries the page's requestAnimationFrame count in its colour, so each
// captured frame maps to the exact page frame that produced it: its time from the close trigger
// and what every layer of the overlay was driven to in that frame (content, fill, frost,
// refraction, glint, CSS rim, shadow, scale, mounted or not). Nothing is slowed down.
//
// Residue: each frame's overlay region against the same region once the overlay has gone (for an
// entry: once it has settled), as the share of visibly different pixels and their mean difference.
// Two numbers decide a clean exit (thresholds in THRESHOLDS):
//   plateauMs  the longest stretch, from the first frame the leaving state reached until removal,
//              in which something is still visibly there while the residue's magnitude stays put
//              (a frozen pane; a slow fade still lowers it)
//   popShare   per run, the residue of the frame captured from that run's last mounted page frame,
//              worst over runs: what vanishes at once when the overlay is removed (a final pop)
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { nativeImage } from "electron";
import { js, launchApp, navigate, setAppearance, setScenario, showDiagnosticsEntry, until } from "./real-ui.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A pixel differs visibly when one of its 8-bit channels differs by more than this. */
const VISIBLE_STEP = 6;
export const THRESHOLDS = {
  /** Something is still on screen while more than this share of the region differs visibly. */
  presentShare: 0.0005,
  /** A stretch is still while the residue's mean (0-255) stays within the larger of these of its start ... */
  plateauMeanStep: 0.02,
  plateauMeanShare: 0.03,
  /** ... and each frame differs visibly from the frame a refresh before in less than this share. */
  stillShare: 0.0002,
  /** Longest acceptable still-but-visible stretch before removal. */
  plateauMs: 20,
  /** Largest acceptable visible residue in the last mounted frame. */
  popShare: 0.0002,
};

// ---------------------------------------------------------------------------------------------
// page side
// ---------------------------------------------------------------------------------------------

const PAGE = `(() => {
  if (window.__exitProbe) return true;
  const marker = document.createElement("div");
  marker.style.cssText = "position:fixed;right:0;bottom:0;width:6px;height:6px;z-index:2147483647;pointer-events:none;background:#000;contain:strict";
  document.documentElement.append(marker);
  const enc = (i) => [(i & 15) * 16 + 8, ((i >> 4) & 15) * 16 + 8, ((i >> 8) & 15) * 16 + 8];
  let n = 0;
  let rec = null;
  const num = (v) => { const x = parseFloat(v); return Number.isFinite(x) ? Math.round(x * 1000) / 1000 : null; };
  const alphaOf = (color) => {
    if (!color || color === "transparent") return 0;
    const m = /\\/\\s*([0-9.]+)\\s*\\)$|rgba\\([^)]*,\\s*([0-9.]+)\\)$/.exec(color);
    return m ? Number(m[1] ?? m[2]) : 1;
  };
  function layers(el) {
    if (!el || !el.isConnected) return { mounted: false };
    const cs = getComputedStyle(el);
    const content = el.querySelector(".lg-overlay-content");
    const ccs = content ? getComputedStyle(content) : null;
    const id = el.style.getPropertyValue("--lg-backdrop").match(/#([^)]*)/)?.[1];
    const f = id ? document.getElementById(id) : null;
    const attr = (sel, name) => num(f?.querySelector(sel)?.getAttribute(name));
    const shadow = cs.boxShadow === "none" ? 0 : Math.max(0, ...[...cs.boxShadow.matchAll(/(?:rgba?|hsla?|oklch|oklab|lab|lch|color)\\([^)]*\\)/g)].map((m) => alphaOf(m[0])));
    const layer = el.closest(".app-dialog-layer");
    return {
      mounted: true,
      leaving: el.dataset.leaving ?? el.closest("[data-leaving]")?.dataset.leaving ?? null,
      opacity: num(cs.opacity),
      scale: cs.transform === "none" ? 1 : Math.round(new DOMMatrixReadOnly(cs.transform).a * 1000) / 1000,
      fill: Math.round(alphaOf(cs.backgroundColor) * 1000) / 1000,
      tint: num(cs.getPropertyValue("--lg-morph-tint")),
      frostTrack: num(cs.getPropertyValue("--lg-morph-frost")),
      rimTrack: num(cs.getPropertyValue("--lg-morph-rim")),
      lensTrack: num(cs.getPropertyValue("--lg-morph-lens")),
      shadow: Math.round(shadow * 1000) / 1000,
      rimCss: num(getComputedStyle(el, "::before").opacity),
      content: ccs ? num(ccs.opacity) : null,
      contentBlur: ccs ? num((/blur\\(([0-9.]+)px\\)/.exec(ccs.filter) ?? [0, 0])[1]) : null,
      backdrop: cs.backdropFilter.slice(0, 12),
      lens: Boolean(f) && cs.backdropFilter.startsWith("url("),
      frost: attr('[result="frost"]', "stdDeviation"),
      bend: attr('[result="bentG"]', "scale"),
      glint: attr('[result="white"]', "flood-opacity"),
      vivid: attr('[result="vivid"]', "values"),
      gain: num((f?.querySelector('[result="gain"]')?.getAttribute("values") ?? "").split(" ")[0]),
      scrim: layer ? Math.round(alphaOf(getComputedStyle(layer).backgroundColor) * 1000) / 1000 : null,
      inert: el.closest("[inert]") !== null,
      pointer: cs.pointerEvents,
    };
  }
  function frame(t) {
    n++;
    const [r, g, b] = enc(n);
    marker.style.background = "rgb(" + r + "," + g + "," + b + ")";
    if (rec) {
      if (!rec.el?.isConnected && rec.selector) {
        const found = document.querySelector(rec.selector);
        if (found && rec.removed === null && rec.entry) rec.el = found;
      }
      const el = rec.el?.isConnected ? rec.el : null;
      // ran: when this callback really ran. After a busy main thread the callback runs long after
      // its frame's timestamp, and the frame it draws is presented then.
      const frame = { n, t: Math.round(t * 100) / 100, ran: Math.round(performance.now() * 100) / 100, ...layers(el) };
      if (rec.entry && el && rec.frames.filter((f) => f.mounted).length < 6) {
        frame.anims = [el, el.querySelector(".lg-overlay-content")].filter(Boolean).flatMap((node) => node.getAnimations())
          .map((a) => ({ p: a.transitionProperty ?? a.animationName, pending: a.pending, start: a.startTime === null ? null : Math.round(a.startTime - t), cur: a.currentTime === null ? null : Math.round(a.currentTime) }));
      }
      rec.frames.push(frame);
    }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
  const onTrigger = (event) => {
    if (!rec || rec.trigger !== null || !rec.armed) return;
    if (event.type === "keydown" && event.key !== "Escape") return;
    if ((event.type === "click") !== (rec.triggerOn === "click")) return;
    rec.trigger = Math.round(event.timeStamp * 100) / 100;
    rec.triggerType = event.type;
    triggered();
  };
  // Optional disturbances after the trigger: a busy main thread, or one layer switched off.
  function triggered() {
    const { stall, ablate } = rec;
    if (stall && stall.at !== "leaving") setTimeout(() => { const end = performance.now() + stall.ms; while (performance.now() < end); }, stall.at);
    if (ablate) { const el = rec.el; setTimeout(() => { if (el?.isConnected) el.style.setProperty(ablate.prop, ablate.value, "important"); }, ablate.at); }
  }
  for (const type of ["keydown", "pointerdown", "contextmenu", "click"]) window.addEventListener(type, onTrigger, true);
  window.__exitProbe = {
    /**
     * Starts logging every frame. Exit: the overlay is the element matched now, followed until it
     * leaves the DOM. Entry: the first element that matches. The trigger is the next Escape or
     * pointer press, or, with leavingTrigger, the moment the element gets data-leaving.
     */
    start(selector, { entry = false, leavingTrigger = false, stall = null, ablate = null, triggerOn = null } = {}) {
      const el = entry ? null : document.querySelector(selector);
      rec = { el, selector, entry, frames: [], trigger: null, triggerType: null, removed: null, armed: !leavingTrigger, stall, ablate, triggerOn, origin: performance.timeOrigin };
      const mo = new MutationObserver(() => {
        if (leavingTrigger && rec.trigger === null && rec.el?.hasAttribute("data-leaving")) {
          rec.trigger = Math.round(performance.now() * 100) / 100;
          rec.triggerType = "leaving:" + rec.el.dataset.leaving;
          triggered();
        }
        if (rec.el && !rec.el.isConnected && rec.removed === null && !entry) rec.removed = Math.round(performance.now() * 100) / 100;
        // A busy main thread inside the close's own task, before anything of the exit is drawn.
        if (rec.stall?.at === "leaving" && !rec.stalled && rec.el?.hasAttribute("data-leaving")) {
          rec.stalled = true;
          const end = performance.now() + rec.stall.ms;
          while (performance.now() < end);
        }
      });
      mo.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["data-leaving"] });
      rec.mo = mo;
      return entry || Boolean(el);
    },
    stop() { const out = rec; rec = null; if (!out) return null; out.mo.disconnect(); delete out.mo; delete out.el; return out; },
    rect(selector) { const el = document.querySelector(selector); if (!el) return null; const { x, y, width, height } = el.getBoundingClientRect(); return { x, y, width, height }; },
    viewport: () => ({ width: innerWidth, height: innerHeight, dpr: devicePixelRatio }),
    focused: () => document.activeElement?.dataset?.testid ?? document.activeElement?.tagName ?? null,
  };
  return true;
})()`;

// ---------------------------------------------------------------------------------------------
// capture and analysis
// ---------------------------------------------------------------------------------------------

/** Subscribes to presented frames, keeping the region (CSS px) and the frame marker of each. */
function subscribe(wc, region, viewport) {
  const frames = [];
  let scale = 0;
  wc.beginFrameSubscription(false, (image) => {
    const size = image.getSize();
    if (!scale) scale = size.width / viewport.width;
    const s = scale;
    const mark = image.crop({ x: size.width - Math.round(3 * s) - 1, y: size.height - Math.round(3 * s) - 1, width: 1, height: 1 }).toBitmap();
    const box = { x: Math.round(region.x * s), y: Math.round(region.y * s), width: Math.round(region.width * s), height: Math.round(region.height * s) };
    frames.push({ at: performance.timeOrigin + performance.now(), mark: [mark[2], mark[1], mark[0]], bitmap: image.crop(box).toBitmap(), w: box.width, h: box.height });
  });
  return { stop() { wc.endFrameSubscription(); return { frames, scale }; } };
}

const decodeMark = ([r, g, b]) => {
  const q = (v) => Math.max(0, Math.min(15, Math.round((v - 8) / 16)));
  return q(r) | (q(g) << 4) | (q(b) << 8);
};

/** Visible-difference statistics between two BGRA bitmaps of equal size, per band mask. */
function compare(a, b, masks) {
  const out = masks.map(() => ({ sum: 0, visible: 0, count: 0 }));
  const length = Math.min(a.length, b.length) / 4;
  for (let p = 0; p < length; p++) {
    const i = p * 4;
    const d = Math.max(Math.abs(a[i] - b[i]), Math.abs(a[i + 1] - b[i + 1]), Math.abs(a[i + 2] - b[i + 2]));
    for (let k = 0; k < masks.length; k++) {
      if (!masks[k][p]) continue;
      const s = out[k];
      s.sum += d;
      s.count++;
      if (d > VISIBLE_STEP) s.visible++;
    }
  }
  return out.map((s) => ({ mean: s.count ? +(s.sum / s.count).toFixed(3) : 0, share: s.count ? +(s.visible / s.count).toFixed(5) : 0 }));
}

const BANDS = ["region", "face", "rim", "outside"];

/**
 * Band masks in device px: the whole region, the surface's inner face (inside its rim), the rim
 * (the outer 26 CSS px of the open surface) and what lies outside the surface (its shadow).
 */
function masksFor(region, surface, scale, w, h, rim = 26) {
  const sx = (surface.x - region.x) * scale, sy = (surface.y - region.y) * scale;
  const sw = surface.width * scale, sh = surface.height * scale, r = rim * scale;
  const masks = BANDS.map(() => new Uint8Array(w * h));
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const p = y * w + x;
    const inside = x >= sx && x < sx + sw && y >= sy && y < sy + sh;
    const face = x >= sx + r && x < sx + sw - r && y >= sy + r && y < sy + sh - r;
    masks[0][p] = 1;
    masks[1][p] = face ? 1 : 0;
    masks[2][p] = inside && !face ? 1 : 0;
    masks[3][p] = inside ? 0 : 1;
  }
  return masks;
}

/**
 * Matches one run's captured frames to its page log. A frame whose marker is new is placed at its
 * page frame's time; a repeated marker is a frame the compositor produced without a new page
 * frame (a compositor-driven animation, or a busy main thread), placed by its capture time less
 * the run's median capture latency.
 */
function matchRun(capture, log) {
  const byN = new Map(log.frames.map((f) => [f.n, f]));
  const logged = log.frames.map((f) => f.n);
  const resolve = (m) => {
    let n = null;
    for (const k of logged) if ((k & 4095) === m && (n === null || k > n)) n = k;
    return n;
  };
  const seen = new Set();
  const shots = [];
  for (const f of capture.frames) {
    const n = resolve(decodeMark(f.mark));
    if (n === null) continue;
    const page = byN.get(n);
    shots.push({ ...f, n, page, fresh: !seen.has(n), pageAt: log.origin + page.t });
    seen.add(n);
  }
  const latencies = shots.filter((s) => s.fresh && !s.page.late).map((s) => s.at - s.pageAt).sort((a, b) => a - b);
  const latency = latencies.length ? latencies[Math.floor(latencies.length / 2)] : 0;
  for (const s of shots) s.t = s.fresh ? s.page.t : s.at - latency - log.origin;
  shots.sort((a, b) => a.t - b.t);
  return { shots, latency: +latency.toFixed(1), repeats: shots.filter((s) => !s.fresh).length };
}

/** The first page frame (ms from the trigger) at which pred holds and keeps holding while mounted. */
function settledAt(frames, t0, pred) {
  // A late frame's layer values belong to its stale timestamp, not to when it was drawn.
  const mounted = frames.filter((f) => f.t >= t0 && f.mounted && !f.late);
  let at = null;
  for (const f of mounted) {
    if (pred(f)) at ??= f.t - t0;
    else at = null;
  }
  return at === null ? null : +at.toFixed(1);
}

const median = (xs, digits = 1) => {
  const v = xs.filter((x) => x !== null && x !== undefined).sort((a, b) => a - b);
  return v.length ? +v[Math.floor((v.length - 1) / 2)].toFixed(digits) : null;
};

/** Layer timings of one run, from every page frame (not only the captured ones). */
function layerTimings(log) {
  const { frames, trigger: t0 } = log;
  const after = frames.filter((f) => f.t >= t0);
  const firstWithout = after.find((f) => !f.mounted);
  const lastMounted = [...after].reverse().find((f) => f.mounted);
  const intervals = after.slice(1).map((f, i) => f.t - after[i].t);
  const contentGone = settledAt(frames, t0, (f) => f.content === 0);
  const fillGone = settledAt(frames, t0, (f) => f.fill === 0);
  // The body (content and fill) is gone; the surface's opacity then is what is left of the pane.
  const bodyGone = contentGone !== null && fillGone !== null ? Math.max(contentGone, fillGone) : null;
  const atBody = bodyGone === null ? null : after.find((f) => !f.late && f.mounted && +(f.t - t0).toFixed(1) === bodyGone);
  const mountedFrames = after.filter((f) => f.mounted && !f.late);
  const firstMotion = mountedFrames.find((f) => f.content > 0 || (f.frost ?? 0) > 0 || f.scale > mountedFrames[0].scale);
  return {
    bodyGone,
    opacityAtBodyGone: atBody ? atBody.opacity : null,
    firstMounted: mountedFrames.length ? +(mountedFrames[0].t - t0).toFixed(1) : null,
    firstMotion: firstMotion ? +(firstMotion.t - t0).toFixed(1) : null,
    contentGone,
    fillGone,
    frostSettled: settledAt(frames, t0, (f) => (f.lens ? f.frost === 0 : f.frostTrack === 0)),
    refractionSettled: settledAt(frames, t0, (f) => !f.lens || f.bend === 0),
    rimGone: settledAt(frames, t0, (f) => (f.rimCss ?? 0) === 0 && (!f.lens || (f.glint ?? 0) === 0)),
    shadowGone: settledAt(frames, t0, (f) => f.shadow === 0),
    scaleSettled: settledAt(frames, t0, (f) => f.scale === lastMounted?.scale),
    domRemoved: log.removed !== null ? +(log.removed - t0).toFixed(1) : null,
    firstFrameWithout: firstWithout ? +(firstWithout.t - t0).toFixed(1) : null,
    lastMountedLayers: lastMounted ? (({ content, fill, frost, bend, glint, rimCss, shadow, scale, opacity, lensTrack, scrim, backdrop, late }) => ({ content, fill, frost, bend, glint, rimCss, shadow, scale, opacity, lensTrack, scrim, backdrop, stale: Boolean(late) }))(lastMounted) : null,
    lateFrames: after.filter((f) => f.late).map((f) => f.late),
    longestFrameMs: intervals.length ? +Math.max(...intervals).toFixed(1) : null,
    frames: after.filter((f) => f.mounted).length,
  };
}

/**
 * Merges the runs of one case. `entry` compares with the settled overlay instead of its absence.
 */
function analyse(runs, { entry = false } = {}) {
  const { region, surface, scale } = runs[0];
  const { w, h } = runs[0].shots.at(-1);
  const masks = masksFor(region, surface, scale, w, h);
  // Each run's reference is its own last frame, long after the overlay left (or settled).
  const references = runs.map((run) => run.shots.at(-1).bitmap);
  const referenceSpread = Math.max(...references.map((ref) => compare(ref, references[0], [masks[0]])[0].share));
  const samples = [];
  runs.forEach((run, k) => {
    const t0 = run.log.trigger;
    const end = run.timings.firstFrameWithout ?? Infinity;
    for (const s of run.shots) {
      const ms = s.t - t0;
      if (ms < -20 || ms > (entry ? 420 : end + 40)) continue;
      const residue = compare(s.bitmap, references[k], masks);
      samples.push({ ms: +ms.toFixed(1), run: k, n: s.n, fresh: s.fresh, mounted: s.page.mounted, bitmap: s.bitmap, residue: Object.fromEntries(BANDS.map((b, i) => [b, residue[i]])), page: s.page });
    }
  });
  samples.sort((a, b) => a.ms - b.ms);
  // Motion against the latest merged frame at least one refresh (6 ms) earlier.
  for (let i = 0; i < samples.length; i++) {
    let j = i - 1;
    while (j >= 0 && samples[i].ms - samples[j].ms < 6) j--;
    samples[i].motion = j >= 0 ? { ...compare(samples[i].bitmap, samples[j].bitmap, [masks[0]])[0], from: samples[j].ms } : null;
  }
  // The exit window starts with the first frame the leaving state reached: before it the picture
  // is the open overlay (input latency, or a main thread busy before the first exit frame).
  const started = entry ? samples.find((s) => s.ms >= 0 && s.mounted) : samples.find((s) => s.ms >= 0 && s.page.leaving);
  const window = samples.filter((s) => started && s.ms >= started.ms && (entry || s.mounted));
  const present = (s) => s.residue.region.share > THRESHOLDS.presentShare;
  // A plateau: visible residue whose magnitude stays within a small tolerance of where the
  // stretch began while hardly a pixel changes from frame to frame. A slow fade still lowers the
  // magnitude; a cross-fade (content out, scrim out) still changes pixels; a frozen pane does neither.
  const tolerance = (mean) => Math.max(THRESHOLDS.plateauMeanStep, mean * THRESHOLDS.plateauMeanShare);
  let plateauMs = 0, plateau = null, anchor = null;
  for (const s of window) {
    if (!present(s)) { anchor = null; continue; }
    const still = s.motion && s.motion.share < THRESHOLDS.stillShare;
    if (anchor && still && Math.abs(s.residue.region.mean - anchor.residue.region.mean) <= tolerance(anchor.residue.region.mean)) {
      if (s.ms - anchor.ms > plateauMs) { plateauMs = +(s.ms - anchor.ms).toFixed(1); plateau = [anchor.ms, s.ms]; }
    } else anchor = s;
  }
  // Pop, per run: the residue of the frame captured from that run's last mounted page frame.
  const pops = entry ? [] : runs.map((run, k) => {
    const t0 = run.log.trigger;
    const lastN = run.log.frames.filter((f) => f.t >= t0 && f.mounted).at(-1)?.n;
    const shot = run.shots.filter((s) => s.n === lastN).at(-1);
    return shot ? { run: k, ms: +(shot.t - t0).toFixed(1), ...compare(shot.bitmap, references[k], [masks[0]])[0] } : null;
  }).filter(Boolean);
  const worstPop = pops.reduce((a, b) => (!a || b.share > a.share ? b : a), null);
  const lastMounted = entry ? null : window.at(-1);
  const lastVisible = [...samples].reverse().find((s) => s.ms >= 0 && present(s) && (entry || s.mounted));
  const firstAfterTrigger = samples.find((s) => s.ms >= 0 && s.page.mounted);
  const compositorFrames = samples.filter((s) => !s.fresh && s.ms >= 0 && s.mounted).length;
  const removal = median(runs.map((r) => r.timings.firstFrameWithout));
  const timings = Object.fromEntries(Object.keys(runs[0].timings).filter((k) => typeof runs[0].timings[k] === "number" || runs[0].timings[k] === null).map((k) => [k, median(runs.map((r) => r.timings[k]))]));
  // What is left when the body is gone, and for how long a pane follows it.
  const bodyGone = median(runs.map((r) => r.timings.bodyGone));
  const near = bodyGone === null ? [] : samples.filter((s) => s.mounted && Math.abs(s.ms - bodyGone) <= 4.2);
  const residueAtBodyGone = near.length ? +(near.reduce((a, s) => a + s.residue.region.share, 0) / near.length).toFixed(5) : null;
  // Entry: how long the first presented frame holds before the picture starts to change (pixels).
  let entryHoldMs;
  if (entry) {
    const holds = runs.map((run) => {
      const t0 = run.log.trigger;
      const shown = run.shots.filter((s) => s.page.mounted && s.t >= t0);
      if (!shown.length) return null;
      const first = shown[0];
      const firstRef = compare(first.bitmap, run.shots.at(-1).bitmap, [masks[0]])[0].mean;
      const moved = shown.find((s) => Math.abs(compare(s.bitmap, run.shots.at(-1).bitmap, [masks[0]])[0].mean - firstRef) > Math.max(THRESHOLDS.plateauMeanStep, firstRef * THRESHOLDS.plateauMeanShare));
      return moved ? { from: +(first.t - t0).toFixed(1), moved: +(moved.t - t0).toFixed(1) } : null;
    }).filter(Boolean);
    // The captured frames by their page frame's place after mounting (0 = first presented frame):
    // equal residue for places 0 and 1 means the first two frames show the same picture.
    const byPlace = new Map();
    for (const run of runs) {
      const t0 = run.log.trigger;
      const order = run.log.frames.filter((f) => f.mounted && !f.late && f.t >= t0).map((f) => f.n);
      const ref = run.shots.at(-1).bitmap;
      for (const s of run.shots.filter((x) => x.fresh && x.page.mounted)) {
        const place = order.indexOf(s.n);
        if (place < 0 || place > 4) continue;
        const r = compare(s.bitmap, ref, masks);
        if (!byPlace.has(place)) byPlace.set(place, []);
        byPlace.get(place).push({ mean: r[0].mean, rim: r[2].share });
      }
    }
    const framesByPlace = Object.fromEntries([...byPlace].sort((a, b) => a[0] - b[0]).map(([k, v]) => [k, { captures: v.length, mean: median(v.map((x) => x.mean)), rimShare: median(v.map((x) => x.rim)) }]));
    entryHoldMs = { framesByPlace, capturedFirstToChange: median(holds.map((h) => h.moved - h.from)), runs: holds.length,
      logFirstMountedToMotion: median(runs.map((r) => r.timings.firstMotion !== null && r.timings.firstMounted !== null ? r.timings.firstMotion - r.timings.firstMounted : null)),
      firstFrames: runs[0].log.frames.filter((f) => f.mounted && f.t >= runs[0].log.trigger).slice(0, 6).map((f) => ({ ms: +(f.t - runs[0].log.trigger).toFixed(1), content: f.content, frost: f.frost, scale: f.scale, tint: f.tint, lens: f.lens, anims: f.anims })) };
  }
  return {
    runs: runs.length,
    samples: samples.length,
    bodyGoneMs: bodyGone,
    opacityAtBodyGone: median(runs.map((r) => r.timings.opacityAtBodyGone), 3),
    residueAtBodyGone,
    entryHoldMs,
    compositorFrames,
    latencyMs: median(runs.map((r) => r.latency)),
    referenceSpread,
    timings: { ...timings, lastVisibleResidue: lastVisible ? lastVisible.ms : null },
    bodyToLastVisibleMs: lastVisible && bodyGone !== null ? +(lastVisible.ms - bodyGone).toFixed(1) : null,
    lastMountedLayers: runs.map((r) => r.timings.lastMountedLayers)[0],
    plateauMs, plateau,
    popShare: worstPop ? worstPop.share : null,
    popMean: worstPop ? worstPop.mean : null,
    popSampleMs: worstPop ? worstPop.ms : null,
    popExactRuns: entry ? undefined : pops.length,
    latestMountedShare: lastMounted ? lastMounted.residue.region.share : null,
    popGapMs: lastMounted && removal !== null ? +(removal - lastMounted.ms).toFixed(1) : null,
    firstFrameShare: entry && firstAfterTrigger ? firstAfterTrigger.residue.region.share : undefined,
    pass: plateauMs <= THRESHOLDS.plateauMs && (entry || (worstPop !== null && worstPop.share <= THRESHOLDS.popShare)),
    samplesList: samples,
    w, h,
  };
}

/** A horizontal strip of evenly spaced merged frames (box-filtered), as JPEG. */
function strip(samples, w, h, { targetW = 140, count = 10, until: last = Infinity } = {}) {
  const pool = samples.filter((s) => s.ms >= 0 && s.ms <= last);
  if (!pool.length) return null;
  const span = pool.at(-1).ms;
  const pick = [];
  for (let i = 0; i < count; i++) {
    const want = (span * i) / (count - 1);
    const best = pool.reduce((a, b) => (Math.abs(b.ms - want) < Math.abs(a.ms - want) ? b : a));
    if (!pick.includes(best)) pick.push(best);
  }
  const k = Math.max(1, Math.floor(w / targetW));
  const tw = Math.floor(w / k), th = Math.floor(h / k), gap = 3;
  const W = pick.length * (tw + gap) - gap;
  const out = Buffer.alloc(W * th * 4, 0);
  for (let i = 3; i < out.length; i += 4) out[i] = 255;
  pick.forEach((s, f) => {
    for (let y = 0; y < th; y++) for (let x = 0; x < tw; x++) {
      let b0 = 0, b1 = 0, b2 = 0;
      for (let dy = 0; dy < k; dy++) for (let dx = 0; dx < k; dx++) {
        const i = ((y * k + dy) * w + x * k + dx) * 4;
        b0 += s.bitmap[i]; b1 += s.bitmap[i + 1]; b2 += s.bitmap[i + 2];
      }
      const o = (y * W + f * (tw + gap) + x) * 4;
      out[o] = b0 / (k * k); out[o + 1] = b1 / (k * k); out[o + 2] = b2 / (k * k);
    }
  });
  return { image: nativeImage.createFromBitmap(out, { width: W, height: th }), ms: pick.map((s) => s.ms) };
}

// ---------------------------------------------------------------------------------------------
// driving the app
// ---------------------------------------------------------------------------------------------

const send = (wc, type, at, button = "left") => wc.sendInputEvent({ type, x: Math.round(at.x), y: Math.round(at.y), button, clickCount: 1 });
async function click(wc, at, button = "left") {
  send(wc, "mouseMove", at);
  send(wc, "mouseDown", at, button);
  await sleep(25);
  send(wc, "mouseUp", at, button);
}
/** Clicks, then moves the pointer off at once, so nothing under it starts a hover during the exit. */
async function clickAway(wc, at) {
  await click(wc, at);
  send(wc, "mouseMove", NEUTRAL);
}
function key(wc, keyCode) {
  wc.sendInputEvent({ type: "keyDown", keyCode });
  wc.sendInputEvent({ type: "keyUp", keyCode });
}
const centre = (r) => ({ x: r.x + r.width / 2, y: r.y + r.height / 2 });
/** A spot on the page with nothing under it: right of the page title. */
const NEUTRAL = { x: 1000, y: 80 };

/**
 * Runs `open` (outside capture), parks the pointer, then records `close` at real speed until the
 * overlay has gone and `after` ms more. Exit runs follow `selector` until it leaves the DOM.
 */
async function recordRun(win, { selector, open, close, region, entry = false, leavingTrigger = false, after = 380, park = true, phase = 0, stall = null, ablate = null, triggerOn = null, css = null, captureDelay = 0 }) {
  const wc = win.webContents;
  // An optional stylesheet for this case only (diagnostic variants).
  await js(win, `(() => { document.getElementById("probe-case-css")?.remove(); const css = ${JSON.stringify(css)}; if (css) document.head.append(Object.assign(document.createElement("style"), { id: "probe-case-css", textContent: css })); return true; })()`);
  if (!entry) await open();
  if (park) {
    send(wc, "mouseMove", NEUTRAL);
    await sleep(320);
  }
  const surface = entry ? null : await js(win, `__exitProbe.rect(${JSON.stringify(selector)})`);
  if (!entry && !surface) throw new Error(`no overlay ${selector}`);
  const viewport = await js(win, "__exitProbe.viewport()");
  const r = region ?? regionAround(surface, viewport);
  await js(win, `__exitProbe.start(${JSON.stringify(selector)}, ${JSON.stringify({ entry, leavingTrigger, stall, ablate, triggerOn })})`);
  // A timer-driven exit: capture only from shortly before it (seconds of buffered frames stall the
  // capture right when it matters), and stagger the capture's start against it.
  if (leavingTrigger) await sleep(captureDelay + phase * 33.4);
  const sub = subscribe(wc, r, viewport);
  // Spread the runs over the capture period (about 33 ms), so merged frames cover every refresh.
  await sleep(70 + phase * 33.4);
  await (entry ? open() : close());
  if (!entry) await until(win, `!document.querySelector(${JSON.stringify(selector)})`, 6000).catch(() => {});
  await sleep(after);
  const capture = sub.stop();
  const log = await js(win, "__exitProbe.stop()");
  if (log.trigger === null) throw new Error(`no trigger seen for ${selector}`);
  for (const f of log.frames) {
    if (f.ran - f.t > 8.4) {
      f.late = +(f.ran - f.t).toFixed(1);
      f.t = f.ran;
    }
  }
  log.frames.sort((a, b) => a.t - b.t);
  const { shots, latency } = matchRun(capture, log);
  const surfaceNow = surface ?? await js(win, `__exitProbe.rect(${JSON.stringify(selector)})`);
  return { region: r, surface: surfaceNow, scale: capture.scale, shots, latency, log, timings: layerTimings(log) };
}

function regionAround(surface, viewport) {
  const x = Math.max(0, Math.floor(surface.x - 36)), y = Math.max(0, Math.floor(surface.y - 24));
  return {
    x, y,
    width: Math.min(viewport.width - 10, Math.ceil(surface.x + surface.width + 36)) - x,
    height: Math.min(viewport.height - 10, Math.ceil(surface.y + surface.height + 80)) - y,
  };
}

/** Runs one case `count` times with the same region and merges the frames. */
async function measureCase(win, spec, count) {
  // EXIT_CASES=regex: run only the cases whose spec name matches.
  if (process.env.EXIT_CASES && !new RegExp(process.env.EXIT_CASES).test(spec.name)) return { result: null };
  const runs = [];
  for (let i = 0; i < count; i++) {
    const run = await recordRun(win, { ...spec, phase: i / count, region: runs[0]?.region ?? spec.region });
    if (run.shots.length < 3) throw new Error(`${spec.name}: only ${run.shots.length} frames matched`);
    runs.push(run);
    if (spec.between) await spec.between();
  }
  const result = analyse(runs, { entry: spec.entry });
  result.theme = await js(win, "document.documentElement.dataset.theme ?? null");
  return { result, runs };
}

/** Opens the track menu with a real right-click on the `row`th recent row's title, over the cover shelf. */
async function openMenuAt(win, row) {
  const at = await js(win, `(() => {
    const rows = document.querySelectorAll('[data-testid="view-home"] [data-testid="virtual-track-row"]');
    const r = rows[${row}].getBoundingClientRect();
    return { x: r.x + 150, y: r.y + r.height / 2 };
  })()`);
  await click(win.webContents, at, "right");
  await until(win, 'document.querySelector(".track-menu[data-lg-lens]:not([data-leaving])") !== null', 3000);
  await sleep(480);
}

async function homeReady(win) {
  await navigate(win, "home");
  await js(win, '__glassProbe.scrollMain(0)');
  await sleep(300);
}

function summary(r) {
  const { bodyGoneMs, opacityAtBodyGone, residueAtBodyGone, bodyToLastVisibleMs, entryHoldMs, pass, plateauMs, plateau, popShare, popMean, popSampleMs, popGapMs, popExactRuns, latestMountedShare, timings, runs, samples, compositorFrames, latencyMs, referenceSpread, firstFrameShare, lastMountedLayers } = r;
  return { pass, plateauMs, plateau, popShare, popMean, popSampleMs, popGapMs, popExactRuns, latestMountedShare, bodyGoneMs, opacityAtBodyGone, residueAtBodyGone, bodyToLastVisibleMs, entryHoldMs, firstFrameShare, timings, lastMountedLayers, runs, samples, compositorFrames, latencyMs, referenceSpread };
}

function compactCurve(samples) {
  return samples.map((s) => ({
    ms: s.ms, run: s.run, mounted: s.mounted, fresh: s.fresh, leaving: s.page.leaving ?? null,
    residue: Object.fromEntries(BANDS.map((b) => [b, s.residue[b].share])),
    mean: s.residue.region.mean,
    motion: s.motion ? s.motion.share : null,
    ...(s.page.mounted ? { layers: (({ content, fill, frost, bend, glint, rimCss, shadow, scale, opacity, scrim, late }) => ({ content, fill, frost, bend, glint, rimCss, shadow, scale, opacity, scrim, stale: Boolean(late) }))(s.page) } : {}),
  }));
}

/**
 * The measurement: track menu exits over the home page's cover shelf and rows (Escape, outside
 * press, an item, a page change), menu entry, toasts (expiry, action, replacement) and the
 * Diagnostics dialog, in both themes; then reopen, row switch and rapid toggling. Writes
 * exit.json and one JPEG strip per case into outDir.
 */
export async function runExit(outDir, args = []) {
  mkdirSync(outDir, { recursive: true });
  const themes = args.filter((a) => a === "dark" || a === "light");
  const only = args.filter((a) => !themes.includes(a) && !/^\d+$/.test(a));
  const count = Number(args.find((a) => /^\d+$/.test(a)) ?? 8);
  const want = (block) => !only.length || only.some((o) => o.startsWith(block) || block.startsWith(o));
  const win = await launchApp();
  const wc = win.webContents;
  wc.setAudioMuted(true);
  await js(win, PAGE);
  const report = { capturedAt: new Date().toISOString(), thresholds: { ...THRESHOLDS, visibleStep: VISIBLE_STEP }, runsPerCase: count, cases: {}, races: {} };
  const keep = (name, result, stripUntil, { noReference = false } = {}) => {
    if (!result) return;
    if (noReference) Object.assign(result, { pass: null, popShare: null, popMean: null, plateauMs: null, plateau: null, note: "the page behind changes too: residue has no reference" });
    report.cases[name] = { theme: result.theme, ...summary(result), ...(noReference ? { note: result.note } : {}), curve: compactCurve(result.samplesList) };
    const s = strip(result.samplesList, result.w, result.h, { until: stripUntil ?? (result.timings.firstFrameWithout ?? 400) + 10 });
    if (s) {
      writeFileSync(join(outDir, `${name}.jpg`), s.image.toJPEG(80));
      report.cases[name].strip = { file: `${name}.jpg`, ms: s.ms };
    }
    // EXIT_PROBE_FRAMES=ms,ms,...: also save the merged frames nearest those times at full size.
    for (const want of (process.env.EXIT_PROBE_FRAMES ?? "").split(",").filter(Boolean).map(Number)) {
      const s = result.samplesList.reduce((x, y) => (Math.abs(y.ms - want) < Math.abs(x.ms - want) ? y : x));
      const swapped = Buffer.from(s.bitmap);
      writeFileSync(join(outDir, `${name}-${Math.round(s.ms)}ms.png`), nativeImage.createFromBitmap(swapped, { width: result.w, height: result.h }).toPNG());
    }
    console.log(name, JSON.stringify(summary(result)));
  };
  const menu = ".track-menu";
  for (const theme of themes.length ? themes : ["dark", "light"]) {
    await setAppearance(win, { theme, reduceTransparency: false, reduceMotion: false });
    if (want("menu")) {
      await homeReady(win);
      // Warm the path once.
      await openMenuAt(win, 1);
      key(wc, "Escape");
      await sleep(500);
      const open = () => openMenuAt(win, 1);
      keep(`menu-entry-${theme}`, (await measureCase(win, { name: "entry", selector: menu, entry: true, open, close: null, after: 420, park: false,
        region: regionAround(await (async () => { await open(); const r = await js(win, `__exitProbe.rect(".track-menu")`); key(wc, "Escape"); await sleep(500); return r; })(), await js(win, "__exitProbe.viewport()")),
        between: async () => { key(wc, "Escape"); await sleep(500); } }, count)).result, 420);
      keep(`menu-escape-${theme}`, (await measureCase(win, { name: "escape", selector: menu, open, close: () => key(wc, "Escape") }, count)).result);
      keep(`menu-outside-${theme}`, (await measureCase(win, { name: "outside", selector: menu, open, close: () => click(wc, NEUTRAL) }, count)).result);
      keep(`menu-item-${theme}`, (await measureCase(win, {
        name: "item", selector: menu, open, park: false, triggerOn: "click",
        close: async () => clickAway(wc, centre(await js(win, `__exitProbe.rect('[data-testid="action-like"]')`))),
        between: () => sleep(400),
      }, count)).result);
      // A busy main thread (200 ms) inside the close's task, right after it, and 40 ms into the exit.
      for (const at of ["leaving", 0, 40]) {
        keep(`menu-stall${at}-${theme}`, (await measureCase(win, { name: `stall-${at}`, selector: menu, open, close: () => key(wc, "Escape"), stall: { at, ms: 200 }, after: 500 }, count)).result);
      }
      // Ablation: the leaving menu's backdrop filter switched off 140 ms after the close.
      keep(`menu-ablate-${theme}`, (await measureCase(win, { name: "ablate", selector: menu, open, close: () => key(wc, "Escape"), ablate: { at: 140, prop: "backdrop-filter", value: "none" } }, 4)).result);
      // Items that also change the page behind the menu (no reference: only layers and frames count).
      const viaItem = (id) => async () => click(wc, centre(await js(win, `__exitProbe.rect('[data-testid="${id}"]')`)));
      keep(`menu-album-${theme}`, (await measureCase(win, { name: "album", selector: menu, park: false, triggerOn: "click", open: async () => { await homeReady(win); await open(); }, close: viaItem("action-go-album"), after: 500 }, 4)).result, undefined, { noReference: true });
      await homeReady(win);
      keep(`menu-play-${theme}`, (await measureCase(win, { name: "play", selector: menu, park: false, triggerOn: "click", open, close: viaItem("action-play"), after: 500 }, 4)).result, undefined, { noReference: true });
      await js(win, '__glassProbe.click("btn-play-pause")').catch(() => {});
      await sleep(400);
    }
    if (want("toast")) {
      await homeReady(win);
      // Selecting a mock scenario restores its default settings, the theme among them.
      await setScenario(win, "indeterminateNext");
      await setAppearance(win, { theme, reduceTransparency: false, reduceMotion: false });
      await homeReady(win);
      const showToast = async () => {
        await js(win, '__glassProbe.click("btn-next")');
        await until(win, 'document.querySelector(".toast[data-lg-lens]:not([data-leaving])")', 3000);
        await sleep(420);
      };
      // The toast's region stops above the player bar, whose clock and progress move between runs.
      await showToast();
      const toastBox = await js(win, `__exitProbe.rect(".toast")`);
      const bar = await js(win, `__exitProbe.rect('[data-testid="player-bar"]')`);
      await until(win, '!document.querySelector(".toast")', 6000);
      const toastRegion = { x: Math.floor(toastBox.x - 36), y: Math.floor(toastBox.y - 24), width: Math.ceil(toastBox.width + 72), height: Math.floor(bar.y - 1 - (toastBox.y - 24)) };
      // An expiry cannot be phased against the capture, so it gets more runs.
      keep(`toast-expiry-${theme}`, (await measureCase(win, { name: "expiry", selector: ".toast", leavingTrigger: true, open: showToast, close: () => sleep(1), after: 700, region: toastRegion, captureDelay: 2000 }, Math.round(count * 1.5))).result);
      keep(`toast-action-${theme}`, (await measureCase(win, {
        name: "action", selector: ".toast", open: showToast, park: false, triggerOn: "click", region: toastRegion,
        close: async () => clickAway(wc, centre(await js(win, `__exitProbe.rect('[data-testid="toast-action"]')`))),
      }, count)).result);
      await setScenario(win, "normal");
      await setAppearance(win, { theme, reduceTransparency: false, reduceMotion: false });
    }
    if (want("dialog")) {
      await showDiagnosticsEntry(win);
      const open = async () => {
        await click(wc, centre(await js(win, `__exitProbe.rect('[data-testid="settings-open-diagnostics"]')`)));
        await until(win, 'document.querySelector(".app-dialog-layer > .lg-overlay[data-lg-lens]")', 3000);
        await sleep(480);
      };
      const selector = ".app-dialog-layer > .lg-overlay";
      keep(`dialog-close-${theme}`, (await measureCase(win, { name: "dialog", selector, open, park: false, triggerOn: "click",
        close: async () => clickAway(wc, centre(await js(win, `__exitProbe.rect('[data-testid="dialog-close-btn"]')`))),
        between: () => sleep(300) }, count)).result);
      keep(`dialog-escape-${theme}`, (await measureCase(win, { name: "dialog", selector, open, close: () => key(wc, "Escape"), between: () => sleep(300) }, count)).result);
    }
  }
  if (only.includes("entry-diag")) report.entryDiag = await entryDiagnosis(win, wc, count);
  if (want("modes")) report.modes = await modeChecks(win, wc, count);
  if (want("races")) report.races = await raceChecks(win, wc, outDir);
  writeFileSync(join(outDir, "exit.json"), JSON.stringify(report));
  console.log(JSON.stringify(Object.fromEntries(Object.entries(report.cases).map(([k, v]) => [k, { pass: v.pass, plateauMs: v.plateauMs, popShare: v.popShare, popGapMs: v.popGapMs }])), null, 1));
}

/** The open menu's state, for the race checks. */
const MENU_STATE = `(() => {
  const menus = [...document.querySelectorAll(".track-menu")];
  const el = menus.at(-1);
  if (!el) return { copies: 0 };
  const cs = getComputedStyle(el);
  const id = el.style.getPropertyValue("--lg-backdrop").match(/#([^)]*)/)?.[1];
  const f = id ? document.getElementById(id) : null;
  return {
    copies: menus.length, leaving: el.dataset.leaving ?? null, inert: el.inert, opacity: Number(cs.opacity),
    scale: cs.transform === "none" ? 1 : new DOMMatrixReadOnly(cs.transform).a,
    frost: Number(f?.querySelector('[result="frost"]')?.getAttribute("stdDeviation")), glint: Number(f?.querySelector('[result="white"]')?.getAttribute("flood-opacity")),
    rim: Number(getComputedStyle(el, "::before").opacity), content: Number(getComputedStyle(el.querySelector(".lg-overlay-content")).opacity),
    label: el.getAttribute("aria-label"), focusedInside: el.contains(document.activeElement), sameNode: el === window.__raceMenu,
  };
})()`;

const atRest = (m) => m.copies === 1 && m.leaving === null && !m.inert && m.opacity === 1 && m.scale === 1 && m.frost === 5 && m.glint === 1 && m.rim === 1 && m.content === 1;

/**
 * Reopen during the exit, switching rows during the exit and while open, rapid toggling, the
 * state at the moment of closing, a dialog reopened during its exit, a toast replaced during its
 * life, and the Settings dialog whose page is left during its exit.
 */
async function raceChecks(win, wc, raceDir) {
  await setAppearance(win, { theme: "dark", reduceTransparency: false, reduceMotion: false });
  await homeReady(win);
  const out = {};
  const rowPoint = (row) => js(win, `(() => { const r = document.querySelectorAll('[data-testid="view-home"] [data-testid="virtual-track-row"]')[${row}].getBoundingClientRect(); return { x: r.x + 150, y: r.y + r.height / 2 }; })()`);
  const rightClick = async (row) => click(wc, await rowPoint(row), "right");
  // Focus is blurred first: a keyboard close leaves the next menu's first item with a focus ring.
  const settledShot = async () => { await js(win, "document.activeElement?.blur(); true"); await sleep(200); const r = await js(win, `__exitProbe.rect(".track-menu")`); return { rect: r, bitmap: (await wc.capturePage(r)).toBitmap() }; };
  const sameAs = (a, b) => { if (!a || !b || a.bitmap.length !== b.bitmap.length) return null; let v = 0; for (let i = 0; i < a.bitmap.length; i += 4) if (Math.max(Math.abs(a.bitmap[i] - b.bitmap[i]), Math.abs(a.bitmap[i + 1] - b.bitmap[i + 1]), Math.abs(a.bitmap[i + 2] - b.bitmap[i + 2])) > VISIBLE_STEP) v++; return +(v / (a.bitmap.length / 4)).toFixed(5); };

  // A fresh menu at rest, for comparison.
  await openMenuAt(win, 1);
  send(wc, "mouseMove", NEUTRAL);
  await sleep(400);
  const fresh = await settledShot();
  // The moment of closing: retired at once and focus back on the opener.
  await js(win, 'window.__raceMenu = document.querySelector(".track-menu"); true');
  key(wc, "Escape");
  // Input arrives asynchronously: read the state once the close has happened.
  await until(win, "window.__raceMenu.hasAttribute('data-leaving') || !window.__raceMenu.isConnected", 2000);
  out.atClose = await js(win, `new Promise((r) => setTimeout(() => {
    const el = window.__raceMenu, b = el.getBoundingClientRect();
    const hit = document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2);
    r({ leaving: el.dataset.leaving ?? null, inert: el.inert, pointer: getComputedStyle(el).pointerEvents, passes: !el.contains(hit), focusedInside: el.contains(document.activeElement), focused: document.activeElement?.className?.slice?.(0, 40) ?? null });
  }, 0))`);
  await sleep(500);

  for (const [name, again] of [["reopenDuringExit", 1], ["switchDuringExit", 2]]) {
    await openMenuAt(win, 1);
    await js(win, 'window.__raceMenu = document.querySelector(".track-menu"); true');
    const firstLabel = await js(win, 'document.querySelector(".track-menu").getAttribute("aria-label")');
    key(wc, "Escape");
    await sleep(60);
    await rightClick(again);
    send(wc, "mouseMove", NEUTRAL);
    await sleep(600);
    const state = await js(win, MENU_STATE);
    const shot = again === 1 ? await settledShot() : null;
    await sleep(1200);
    const later = await js(win, MENU_STATE);
    if (shot) { writeFileSync(join(raceDir, "race-fresh.png"), nativeImage.createFromBitmap(Buffer.from(fresh.bitmap), { width: fresh.rect.width * 2 | 0, height: fresh.rect.height * 2 | 0 }).toPNG()); writeFileSync(join(raceDir, "race-reopen.png"), (await wc.capturePage(shot.rect)).toPNG()); }
    out[name] = { state, laterCopies: later.copies, laterLeaving: later.leaving, labelChanged: state.label !== firstLabel, residueVsFresh: shot ? sameAs(shot, fresh) : null,
      pass: atRest(state) && state.sameNode && state.focusedInside && later.copies === 1 && later.leaving === null && (again === 1 ? sameAs(shot, fresh) <= 0.0005 : state.label !== firstLabel) };
    key(wc, "Escape");
    await sleep(500);
  }
  // Another row's menu while one is open (the press outside closes, the right-click opens).
  await openMenuAt(win, 1);
  const firstLabel = await js(win, 'document.querySelector(".track-menu").getAttribute("aria-label")');
  await rightClick(2);
  send(wc, "mouseMove", NEUTRAL);
  await sleep(600);
  const switched = await js(win, MENU_STATE);
  await sleep(1200);
  const switchedLater = await js(win, MENU_STATE);
  out.switchWhileOpen = { state: switched, laterCopies: switchedLater.copies, pass: atRest(switched) && switched.label !== firstLabel && switched.focusedInside && switchedLater.copies === 1 && switchedLater.leaving === null };
  key(wc, "Escape");
  await sleep(500);
  // Rapid toggling, ending open; then one close must still remove it.
  for (let i = 0; i < 6; i++) {
    await rightClick(1);
    await sleep(40);
    key(wc, "Escape");
    await sleep(40);
  }
  await rightClick(1);
  send(wc, "mouseMove", NEUTRAL);
  await sleep(600);
  const toggled = await js(win, MENU_STATE);
  await sleep(1000);
  const toggledLater = await js(win, MENU_STATE);
  key(wc, "Escape");
  const closedAt = Date.now();
  await until(win, '!document.querySelector(".track-menu")', 3000).catch(() => {});
  out.rapidToggle = { state: toggled, laterCopies: toggledLater.copies, removedAfterMs: Date.now() - closedAt,
    pass: atRest(toggled) && toggledLater.copies === 1 && toggledLater.leaving === null && Date.now() - closedAt < 800 };
  await sleep(300);

  // Dialog reopened during its exit.
  await showDiagnosticsEntry(win);
  const openDialog = async () => {
    await click(wc, centre(await js(win, `__exitProbe.rect('[data-testid="settings-open-diagnostics"]')`)));
    await until(win, 'document.querySelector(".app-dialog-layer > .lg-overlay[data-lg-lens]:not([data-leaving])")', 3000);
  };
  await openDialog();
  await sleep(500);
  await js(win, 'window.__raceDialog = document.querySelector("[data-testid=accessible-dialog]"); true');
  key(wc, "Escape");
  await until(win, 'document.querySelector(".app-dialog-layer[data-leaving]")', 2000);
  const dialogAtClose = await js(win, `({ focused: document.activeElement?.dataset?.testid ?? null, inert: document.querySelector(".app-dialog-layer").inert })`);
  await sleep(60);
  await openDialog();
  await sleep(600);
  const dialogState = await js(win, `(() => { const el = document.querySelector(".app-dialog-layer > .lg-overlay"); const cs = getComputedStyle(el); return { copies: document.querySelectorAll(".app-dialog-layer").length, sameNode: el === window.__raceDialog, leaving: el.closest("[data-leaving]") !== null, opacity: Number(cs.opacity), scale: cs.transform === "none" ? 1 : new DOMMatrixReadOnly(cs.transform).a, focused: document.activeElement?.dataset?.testid ?? null }; })()`);
  await sleep(1200);
  const dialogLater = await js(win, 'document.querySelectorAll(".app-dialog-layer").length');
  out.dialogReopen = { atClose: dialogAtClose, state: dialogState, laterCopies: dialogLater,
    pass: dialogAtClose.focused === "settings-open-diagnostics" && dialogAtClose.inert && dialogState.copies === 1 && dialogState.sameNode && !dialogState.leaving && dialogState.opacity === 1 && dialogState.scale === 1 && dialogLater === 1 };
  key(wc, "Escape");
  await sleep(500);

  // The Settings dialog closed and its page left 50 ms later: the parent unmounts mid-exit.
  await showDiagnosticsEntry(win);
  const logout = await js(win, `__exitProbe.rect('[data-testid="settings-logout"]')`);
  if (logout) {
    await js(win, `document.querySelector('[data-testid="settings-logout"]').scrollIntoView({ block: "center" }); true`);
    await sleep(250);
    await click(wc, centre(await js(win, `__exitProbe.rect('[data-testid="settings-logout"]')`)));
    await until(win, 'document.querySelector(".app-dialog-layer > .lg-overlay[data-lg-lens]")', 3000);
    await sleep(500);
    await js(win, `__exitProbe.start(".app-dialog-layer > .lg-overlay", {})`);
    key(wc, "Escape");
    await sleep(50);
    await click(wc, centre(await js(win, `__exitProbe.rect('[data-testid="nav-home"]')`)));
    await sleep(500);
    const log = await js(win, "__exitProbe.stop()");
    const t = layerTimings(log);
    // Not cut short: removed no earlier than its 160 ms clock, with its lens to the end.
    out.parentUnmount = { timings: t, pass: t.domRemoved >= 160 && t.lastMountedLayers !== null && t.lastMountedLayers.backdrop.startsWith("url(") && t.lastMountedLayers.opacity <= 0.1 };
  }

  // A toast replaced during its life: the old one leaves under the new one.
  await setScenario(win, "indeterminateNext");
  await setAppearance(win, { theme: "dark", reduceTransparency: false, reduceMotion: false });
  await homeReady(win);
  await js(win, '__glassProbe.click("btn-next")');
  await until(win, 'document.querySelector(".toast[data-lg-lens]:not([data-leaving])")', 3000);
  await sleep(500);
  await js(win, `__exitProbe.start(".toast", { leavingTrigger: true })`);
  await js(win, '__glassProbe.click("btn-next")');
  await until(win, 'document.querySelectorAll(".toast").length === 1 && !document.querySelector(".toast[data-leaving]")', 3000).catch(() => {});
  await sleep(300);
  const tlog = await js(win, "__exitProbe.stop()");
  const tt = layerTimings(tlog);
  out.toastReplaced = { timings: tt, pass: tt.lastMountedLayers !== null && tt.lastMountedLayers.opacity <= 0.02 && tt.domRemoved !== null };
  await setScenario(win, "normal");
  console.log("races", JSON.stringify(Object.fromEntries(Object.entries(out).map(([k, v]) => [k, v.pass ?? v]))));
  return out;
}

/** The app's reduced-motion and reduced-transparency settings: a menu exit in each. */
async function modeChecks(win, wc, count) {
  const out = {};
  for (const [name, mode] of [["reduceMotion", { reduceMotion: true, reduceTransparency: false }], ["reduceTransparency", { reduceMotion: false, reduceTransparency: true }]]) {
    await setAppearance(win, { theme: "dark", ...mode });
    await homeReady(win);
    const open = async () => {
      await click(wc, await js(win, `(() => { const r = document.querySelectorAll('[data-testid="view-home"] [data-testid="virtual-track-row"]')[1].getBoundingClientRect(); return { x: r.x + 150, y: r.y + r.height / 2 }; })()`), "right");
      await until(win, 'document.querySelector(".track-menu:not([data-leaving])") !== null', 3000);
      await sleep(400);
    };
    const { result } = await measureCase(win, { name, selector: ".track-menu", open, close: () => key(wc, "Escape") }, count);
    out[name] = { ...summary(result), curve: compactCurve(result.samplesList) };
    console.log(`mode-${name}`, JSON.stringify(summary(result)));
  }
  await setAppearance(win, { theme: "dark", reduceMotion: false, reduceTransparency: false });
  return out;
}

/** Entry with only the sampled optical tracks: no scale and no content transition (diagnostic). */
const ENTRY_TRACKS_ONLY = `
html:root body .track-menu.track-menu { transition: --lg-morph-frost 150ms cubic-bezier(0, 0.9, 0.2, 1), --lg-morph-tint 280ms cubic-bezier(0.3, 0.2, 0.6, 1), --lg-morph-rim 280ms linear !important; }
html:root body .track-menu.track-menu > .lg-overlay-content { transition: none !important; }`;

/** Why an opening menu holds its first frame: the shipped entry, and one with only the sampled tracks. */
async function entryDiagnosis(win, wc, count) {
  await setAppearance(win, { theme: "dark", reduceTransparency: false, reduceMotion: false });
  await homeReady(win);
  const open = () => openMenuAt(win, 1);
  await open();
  const region = regionAround(await js(win, `__exitProbe.rect(".track-menu")`), await js(win, "__exitProbe.viewport()"));
  key(wc, "Escape");
  await sleep(500);
  const out = {};
  for (const [name, css] of [["shipped", null], ["tracksOnly", ENTRY_TRACKS_ONLY]]) {
    const { result } = await measureCase(win, { name: `entry-${name}`, selector: ".track-menu", entry: true, open, close: null, after: 420, park: false, region, css,
      between: async () => { key(wc, "Escape"); await sleep(500); } }, count);
    out[name] = { plateauMs: result.plateauMs, entryHoldMs: result.entryHoldMs };
    console.log(`entry-${name}`, JSON.stringify(out[name]).slice(0, 1500));
  }
  await js(win, 'document.getElementById("probe-case-css")?.remove(); true');
  return out;
}
