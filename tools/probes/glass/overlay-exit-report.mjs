#!/usr/bin/env node
// Writes the compact overlay-exit evidence from two `exit` probe runs (overlay-exit.mjs):
//
//   node tools/probes/glass/overlay-exit-report.mjs <before-dir> <after-dir> [out-dir]
//
// Each directory holds an exit.json and its strips. The output (default
// tools/probes/glass/out/exit-report, which git ignores) gets overlay-exit.json with both runs'
// timings, pass numbers and residue curves, and a before/after pair of strips for a few cases.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const [beforeDir, afterDir, outArg] = process.argv.slice(2);
if (!beforeDir || !afterDir) {
  console.error("usage: overlay-exit-report.mjs <before-dir> <after-dir> [out-dir]");
  process.exit(2);
}
const outDir = resolve(outArg ?? join(import.meta.dirname, "out/exit-report"));
const read = (dir) => JSON.parse(readFileSync(join(dir, "exit.json"), "utf8"));
const before = read(beforeDir);
const after = read(afterDir);
mkdirSync(outDir, { recursive: true });

const round = (x, d = 1) => (x === null || x === undefined ? null : +Number(x).toFixed(d));
const brief = (c) => c && {
  theme: c.theme ?? null,
  pass: c.pass, plateauMs: c.plateauMs, plateau: c.plateau, popShare: c.popShare, popMean: c.popMean,
  popSampleMs: c.popSampleMs, popExactRuns: c.popExactRuns, latestMountedShare: c.latestMountedShare,
  bodyGoneMs: c.bodyGoneMs, opacityAtBodyGone: c.opacityAtBodyGone, residueAtBodyGone: c.residueAtBodyGone,
  bodyToLastVisibleMs: c.bodyToLastVisibleMs,
  ...(c.entryHoldMs ? { entryHold: { logFirstMountedToMotion: c.entryHoldMs.logFirstMountedToMotion, framesByPlace: c.entryHoldMs.framesByPlace } } : {}),
  runs: c.runs, samples: c.samples, compositorFrames: c.compositorFrames, referenceSpread: c.referenceSpread,
  ...(c.note ? { note: c.note } : {}),
  timings: c.timings, lastMountedLayers: c.lastMountedLayers,
};
// [ms, mounted, region share, region mean, rim-band share, surface opacity (page log), fresh page frame]
const curve = (c) => c?.curve?.filter((s) => s.ms >= -10).map((s) => [round(s.ms), s.mounted ? 1 : 0, round(s.residue.region, 5), round(s.mean, 3), round(s.residue.rim, 5), s.layers && !s.layers.stale ? s.layers.opacity ?? null : null, s.fresh ? 1 : 0]);
const races = (r) => r && Object.fromEntries(Object.entries(r).map(([k, v]) => [k, k === "atClose" ? v : {
  pass: v.pass,
  ...(v.timings ? { domRemoved: v.timings.domRemoved, lastMountedLayers: v.timings.lastMountedLayers } : {}),
  ...(v.residueVsFresh !== undefined ? { residueVsFresh: v.residueVsFresh } : {}),
  ...(v.removedAfterMs !== undefined ? { removedAfterMs: v.removedAfterMs } : {}),
}]));

/**
 * The pane left when the body (content and fill) is gone, from the per-frame curve: the surface's
 * opacity (page log, fresh frames) and the residue share of mounted frames within one refresh.
 */
function atBodyGone(c) {
  if (!c?.curve || c.bodyGoneMs == null) return null;
  const near = c.curve.filter((s) => s.mounted && Math.abs(s.ms - c.bodyGoneMs) <= 8.4);
  const opacities = near.filter((s) => s.fresh && s.layers && !s.layers.stale).map((s) => s.layers.opacity);
  const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
  return {
    bodyGoneMs: c.bodyGoneMs,
    surfaceOpacity: round(mean(opacities), 3),
    residueShare: round(mean(near.map((s) => s.residue.region)), 4),
    frames: near.length,
    bodyToLastVisibleMs: c.bodyToLastVisibleMs,
  };
}

const cases = Object.fromEntries(Object.keys(after.cases).map((name) => [name, {
  paneAfterBody: { before: atBodyGone(before.cases[name]), after: atBodyGone(after.cases[name]) },
  before: brief(before.cases[name]), after: brief(after.cases[name]),
  curveBefore: curve(before.cases[name]), curveAfter: curve(after.cases[name]),
}]));
const report = {
  about: "Overlay exits measured from the real window's presented frames at real speed (tools/probes/glass/overlay-exit.mjs, mode exit), mock mode, muted, throwaway profile. Before: the UI without this change (loaded through YTMCP_UI_DIST_DIR); after: with it.",
  thresholds: after.thresholds,
  definitions: {
    residue: "share of the region's device pixels whose largest channel differs by more than visibleStep from the same region once the overlay has gone (entry: once settled); mean is the mean difference (0-255)",
    plateauMs: "longest stretch, from the first frame with the leaving state to removal, with residue above presentShare whose mean stays within max(plateauMeanStep, plateauMeanShare x mean) of its start while each frame differs from the frame a refresh before in under stillShare of the pixels",
    popShare: "worst, over runs, residue of the frame captured from that run's last mounted page frame (popExactRuns: runs where that exact frame was captured)",
    paneAfterBody: "bodyGoneMs: when content and fill have both reached 0 (median over runs); surfaceOpacity and residueShare: means over the mounted frames within one refresh of it (opacity from fresh page frames); bodyToLastVisibleMs: from then to the last frame with visible residue. The dialog's residue includes its fading scrim",
    timings: "ms from the close trigger (Escape keydown, pointerdown, click, or data-leaving for an expiry), medians over runs, from every page frame's layer log; a page frame whose callback ran more than a refresh after its timestamp (after a busy main thread) is placed when it ran and its layer values are not used",
    curve: "[ms, mounted, region share, region mean, rim-band share, surface opacity from the page log (null when stale), fresh page frame (0 = drawn by the compositor without a new page frame)]",
  },
  runsPerCase: after.runsPerCase,
  cases,
  races: { before: races(before.races), after: races(after.races) },
  modes: after.modes && Object.fromEntries(Object.entries(after.modes).map(([k, v]) => [k, brief(v)])),
  entryDiagnosis: after.entryDiag ?? null,
};
writeFileSync(join(outDir, "overlay-exit.json"), JSON.stringify(report));

const strips = ["menu-escape-dark", "menu-escape-light", "menu-stall40-dark", "toast-expiry-light", "dialog-close-light"];
for (const name of strips) {
  for (const [label, dir] of [["before", beforeDir], ["after", afterDir]]) {
    try {
      copyFileSync(join(dir, `${name}.jpg`), join(outDir, `${name}-${label}.jpg`));
    } catch (error) {
      console.error(String(error));
    }
  }
}
for (const [name, c] of Object.entries(cases)) {
  const a = c.after;
  const pane = c.paneAfterBody.after;
  console.log(`${name}: before pass=${c.before?.pass} plateau=${c.before?.plateauMs} pop=${c.before?.popShare} | after pass=${a?.pass} plateau=${a?.plateauMs} pop=${a?.popShare} exact=${a?.popExactRuns} latest=${a?.latestMountedShare} | body=${pane?.bodyGoneMs} opacity=${pane?.surfaceOpacity} residue=${pane?.residueShare} n=${pane?.frames} bodyToLast=${pane?.bodyToLastVisibleMs}`);
}
