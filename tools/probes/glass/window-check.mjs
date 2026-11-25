// Window checks of the UI fluency fixes in the real application (mock mode), with probe-only
// latency and reply shaping injected into the page's fake core at run time. Nothing under ui/src
// knows about it: the fake core's instance is caught once through a side-effect-only conditional
// breakpoint (CDP), then its prototype methods are wrapped from the page.
//
//   node tools/probes/glass/run-probe.mjs check [feedback|nav|write|nowplaying|search|lyrics|lists|seek ...]
//   node tools/probes/glass/run-probe.mjs check [covers|titles|menu|preparing|recent|problems|paint|leave|libback ...]
//   covers (real artwork, app scheme re-handled, reload), problems (minimises the window, which
//   KWin leaves unpainted afterwards) and recent (reload) change the window for later sections:
//   run each of them alone.
//
// Each section writes out/check/<section>.json as it finishes; screenshots go next to them.
// Every reply the probe shapes (extra playlists, search groups, paging, queue advance, longer
// lyrics, add-to-playlist) is listed under "shaped" in the section's JSON.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { extname, join, resolve } from "node:path";
import { nativeImage } from "electron";
import { js, launchApp, navigate, openLongList, PAGE_HELPERS, setAppearance, until } from "./real-ui.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const round = (v, d = 2) => (v == null || Number.isNaN(v) ? v : Number(v.toFixed(d)));

/** Runs a page-side function (serialised) with JSON arguments. */
const call = (win, fn, ...args) => js(win, `(${fn.toString()})(...${JSON.stringify(args)})`);

// ---------------------------------------------------------------------------------------------
// fake-core hook
// ---------------------------------------------------------------------------------------------

/** Catches the page's FakeCoreClient through a conditional breakpoint that never pauses. */
async function catchFake(win) {
  const dbg = win.webContents.debugger;
  if (!dbg.isAttached()) dbg.attach("1.3");
  const scripts = [];
  const onMessage = (_e, method, params) => { if (method === "Debugger.scriptParsed") scripts.push(params); };
  dbg.on("message", onMessage);
  await dbg.sendCommand("Debugger.enable");
  await sleep(200);
  dbg.removeListener("message", onMessage);
  const marker = "sendPlayerCommand(e){let t=this.seenCommands";
  let placed = null;
  for (const s of scripts.filter((s) => /index-.*\.js/.test(s.url))) {
    const { scriptSource } = await dbg.sendCommand("Debugger.getScriptSource", { scriptId: s.scriptId });
    const at = scriptSource.indexOf(marker);
    if (at < 0) continue;
    const bodyAt = at + marker.indexOf("{") + 1;
    const before = scriptSource.slice(0, bodyAt);
    const lineNumber = before.split("\n").length - 1;
    const columnNumber = bodyAt - (before.lastIndexOf("\n") + 1);
    const condition = "(globalThis.__probeFake = this, false)";
    placed = await dbg.sendCommand("Debugger.setBreakpoint", { location: { scriptId: s.scriptId, lineNumber, columnNumber }, condition });
    break;
  }
  if (!placed) throw new Error("fake core marker not found in the UI bundle");
  // Any player command reaches it: mute, then unmute.
  await js(win, `(() => { document.querySelector('[data-testid="btn-mute"]').click(); return true; })()`);
  await sleep(300);
  await js(win, `(() => { document.querySelector('[data-testid="btn-mute"]').click(); return true; })()`);
  await until(win, "globalThis.__probeFake", 4000);
  await dbg.sendCommand("Debugger.removeBreakpoint", { breakpointId: placed.breakpointId });
  await dbg.sendCommand("Debugger.disable");
  await call(win, installHook);
}

/**
 * Page side: wraps the fake core's prototype so each call can wait (the wait comes before the
 * original runs, because the fake emits its deltas inside the call) and replies can be shaped.
 */
function installHook() {
  const fake = globalThis.__probeFake;
  const proto = Object.getPrototypeOf(fake);
  const cfg = (globalThis.__probe = {
    latency: { command: 0, mutation: 0, catalog: {}, lyrics: 0 },
    rejectMutation: null,
    shape: {},
    log: [],
    fake,
  });
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  // A command whose op is in cfg.failOps fails as core reports it: accepted, then failed.
  const commandOriginal = proto.sendPlayerCommand;
  proto.sendPlayerCommand = function (cmd) {
    const ms = cfg.latency.command;
    cfg.log.push({ t: performance.now(), name: "sendPlayerCommand", arg: cmd, ms });
    const run = () => {
      if (cfg.failOps?.includes(cmd.op)) {
        const s = this.snapshot;
        const receipt = { commandId: cmd.commandId, op: cmd.op, status: "failed", acceptedAtMs: Date.now(), observedAtMs: null, deduplicated: false,
          error: { code: "TIMEOUT", retriable: false, retryAfterMs: null, safeMessage: "Probe: the player did not confirm the command", requestId: "probe-req-1" } };
        s.pending = { ...s.pending, pendingCommandCount: 0, activeOp: null, lastReceipt: receipt };
        this.emit({ deltaType: "commandUpdated", revision: ++s.revision, receipt });
        return receipt;
      }
      const receipt = commandOriginal.call(this, cmd);
      return cfg.shape.afterCommand ? cfg.shape.afterCommand(this, cmd, receipt) ?? receipt : receipt;
    };
    return ms > 0 ? wait(ms).then(run) : run();
  };
  const recentOriginal = proto.recentSearches;
  if (recentOriginal) {
    proto.recentSearches = function (req, epoch) {
      const ms = cfg.latency.recent ?? 0;
      cfg.log.push({ t: performance.now(), name: "recentSearches", arg: req, ms });
      return ms > 0 ? wait(ms).then(() => recentOriginal.call(this, req, epoch)) : recentOriginal.call(this, req, epoch);
    };
  }
  // A shaped or rejected write never reaches the original, so the mock account stays as it was.
  const mutationOriginal = proto.sendMutation;
  proto.sendMutation = function (req, epoch) {
    const ms = cfg.latency.mutation;
    cfg.log.push({ t: performance.now(), name: "sendMutation", arg: req, ms });
    const run = () => {
      const shaped = cfg.shape.mutation?.(this, req);
      if (shaped) return shaped;
      if (cfg.rejectMutation && req.kind === cfg.rejectMutation) {
        return { kind: req.kind, outcome: "rejected", error: { code: "NETWORK_OFFLINE", safeMessage: "Probe: rejected write", retriable: false } };
      }
      return mutationOriginal.call(this, req, epoch);
    };
    return ms > 0 ? wait(ms).then(run) : run();
  };
  // The original's reply is computed after the wait, so a shaped reply sees the current state.
  const catalogOriginal = proto.queryCatalog;
  proto.queryCatalog = function (req) {
    const ms = cfg.latency.catalog[req.kind] ?? cfg.latency.catalog["*"] ?? 0;
    cfg.log.push({ t: performance.now(), name: "queryCatalog", arg: req, ms });
    const run = () => { const res = catalogOriginal.call(this, req); return cfg.shape.catalog ? cfg.shape.catalog(this, req, res) ?? res : res; };
    return ms > 0 ? wait(ms).then(run) : run();
  };
  const lyricsOriginal = proto.queryLyrics;
  proto.queryLyrics = function (req) {
    const ms = cfg.latency.lyrics;
    cfg.log.push({ t: performance.now(), name: "queryLyrics", arg: { videoId: req.track?.videoId }, ms });
    const run = () => { const res = lyricsOriginal.call(this, req); return cfg.shape.lyrics ? cfg.shape.lyrics(this, req, res) ?? res : res; };
    return ms > 0 ? wait(ms).then(run) : run();
  };
  // Per-frame recorder and layout-shift log.
  globalThis.__rec = { frames: [], marks: [], running: false };
  globalThis.__shifts = [];
  new PerformanceObserver((list) => {
    for (const e of list.getEntries()) {
      globalThis.__shifts.push({ t: e.startTime, value: e.value, hadRecentInput: e.hadRecentInput, sources: e.sources.map((s) => s.node?.dataset?.testid ?? s.node?.className?.toString?.().split(" ")[0] ?? s.node?.nodeName ?? "?") });
    }
  }).observe({ type: "layout-shift", buffered: false });
  for (const type of ["pointerdown", "click", "keydown"]) addEventListener(type, (e) => globalThis.__rec.running && globalThis.__rec.marks.push({ type, t: performance.now(), target: e.target.closest("[data-testid]")?.dataset.testid ?? null }), true);
  return true;
}

/** Starts sampling `sampler` (a page-side function source) once per frame. */
async function startRec(win, sampler) {
  await js(win, `(() => {
    const sample = ${sampler};
    const rec = globalThis.__rec = { frames: [], marks: [], running: true, t0: performance.now(), shiftsFrom: globalThis.__shifts.length };
    const frame = (now) => { if (!rec.running) return; try { rec.frames.push({ t: now - rec.t0, ...sample() }); } catch (err) { rec.frames.push({ t: now - rec.t0, error: String(err) }); } requestAnimationFrame(frame); };
    requestAnimationFrame(frame);
    return true;
  })()`);
}

/** Stops the recorder; times are relative to its start, marks (presses) too. */
async function stopRec(win) {
  return js(win, `(() => {
    const rec = globalThis.__rec; rec.running = false;
    const shifts = globalThis.__shifts.slice(rec.shiftsFrom).map((s) => ({ ...s, t: s.t - rec.t0 }));
    return { frames: rec.frames, marks: rec.marks.map((m) => ({ ...m, t: m.t - rec.t0 })), shifts, cls: shifts.reduce((n, s) => n + s.value, 0), clsWithoutInput: shifts.filter((s) => !s.hadRecentInput).reduce((n, s) => n + s.value, 0) };
  })()`);
}

const setProbe = (win, patch) => js(win, `(() => { Object.assign(globalThis.__probe, ${patch}); return true; })()`);
const setLatency = (win, latency) => js(win, `(() => { Object.assign(globalThis.__probe.latency, ${JSON.stringify(latency)}); return true; })()`);

// ---------------------------------------------------------------------------------------------
// input, geometry, capture
// ---------------------------------------------------------------------------------------------

async function box(win, selector) {
  return js(win, `(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return null; const r = el.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; })()`);
}

/** A real pointer click (move, press, release) at an element's centre; the pointer stays there. */
async function realClick(win, selector, { clickCount = 1 } = {}) {
  const r = await box(win, selector);
  if (!r) throw new Error(`no element ${selector}`);
  const x = Math.round(r.x + r.w / 2), y = Math.round(r.y + r.h / 2);
  const wc = win.webContents;
  wc.sendInputEvent({ type: "mouseMove", x, y });
  wc.sendInputEvent({ type: "mouseDown", x, y, button: "left", clickCount });
  wc.sendInputEvent({ type: "mouseUp", x, y, button: "left", clickCount });
  return { x, y };
}

async function key(win, keyCode, modifiers = []) {
  win.webContents.sendInputEvent({ type: "keyDown", keyCode, modifiers });
  win.webContents.sendInputEvent({ type: "keyUp", keyCode, modifiers });
  await sleep(150);
}

async function grab(win, rect) {
  const image = await win.webContents.capturePage(rect);
  const bitmap = image.toBitmap();
  const pw = image.getSize().width;
  return { image, pw, ph: bitmap.length / 4 / pw, px: (x, y) => { const i = (y * pw + x) * 4; return [bitmap[i + 2], bitmap[i + 1], bitmap[i]]; } };
}

const toLinear = (v) => { const c = v / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
const lum = (c) => 0.2126 * toLinear(c[0]) + 0.7152 * toLinear(c[1]) + 0.0722 * toLinear(c[2]);
const ratio = (a, b) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);

/**
 * Contrast of an icon against what is behind it, from the window's pixels: the icon's box is
 * captured with the icon and with it hidden; the glyph's core is the pixels that changed most
 * (at least 60% of the largest change), which leaves out antialiased edges. Reports the median
 * and the 10th percentile of the per-pixel ratio over that core.
 */
async function iconContrast(win, buttonSelector, saveAs) {
  const icon = `${buttonSelector} svg`;
  const r = await box(win, icon);
  if (!r) return { error: `no icon ${icon}` };
  const rect = { x: Math.floor(r.x) - 1, y: Math.floor(r.y) - 1, width: Math.ceil(r.w) + 2, height: Math.ceil(r.h) + 2 };
  const state = await js(win, `(() => { const b = document.querySelector(${JSON.stringify(buttonSelector)}); const s = b.querySelector("svg"); return { pending: b.hasAttribute("data-pending"), opacity: getComputedStyle(s).opacity, icon: s.dataset.icon, filled: s.hasAttribute("data-filled"), hovered: b.matches(":hover") }; })()`);
  const withIcon = await grab(win, rect);
  await js(win, `(() => { document.querySelector(${JSON.stringify(icon)}).style.visibility = "hidden"; return true; })()`);
  await sleep(80);
  const without = await grab(win, rect);
  await js(win, `(() => { document.querySelector(${JSON.stringify(icon)}).style.visibility = ""; return true; })()`);
  if (saveAs) writeFileSync(saveAs, withIcon.image.resize({ width: withIcon.pw * 4, quality: "good" }).toPNG());
  const samples = [];
  let maxDiff = 0;
  for (let y = 0; y < withIcon.ph; y++) for (let x = 0; x < withIcon.pw; x++) {
    const a = withIcon.px(x, y), b = without.px(x, y);
    const d = Math.abs(lum(a) - lum(b));
    samples.push({ d, a, b });
    if (d > maxDiff) maxDiff = d;
  }
  const core = samples.filter((s) => s.d >= maxDiff * 0.6);
  const ratios = core.map((s) => ratio(lum(s.a), lum(s.b))).sort((p, q) => p - q);
  const mean = (arr, i) => Math.round(arr.reduce((n, s) => n + s[i === "a" ? "a" : "b"].reduce((m, v) => m + v, 0) / 3, 0) / arr.length);
  // The solid part of the glyph (90% of the largest change and up): stroke centres, no edges.
  const solid = samples.filter((s) => s.d >= maxDiff * 0.9).map((s) => ratio(lum(s.a), lum(s.b))).sort((p, q) => p - q);
  return {
    ...state,
    solidPixels: solid.length,
    solidMedian: round(solid[Math.floor(solid.length / 2)]),
    solidMin: round(solid[0]),
    corePixels: core.length,
    median: round(ratios[Math.floor(ratios.length / 2)]),
    p10: round(ratios[Math.floor(ratios.length * 0.1)]),
    min: round(ratios[0]),
    inkGrey: mean(core, "a"),
    behindGrey: mean(core, "b"),
  };
}

const json = (dir, name, data) => writeFileSync(join(dir, `${name}.json`), `${JSON.stringify(data, null, 2)}\n`);

// ---------------------------------------------------------------------------------------------
// 3. immediate feedback
// ---------------------------------------------------------------------------------------------

const PLAY_SAMPLER = `() => { const b = document.querySelector('[data-testid="btn-play-pause"]'); const s = b.querySelector("svg"); return { icon: s.dataset.icon, pending: b.hasAttribute("data-pending"), opacity: Number(getComputedStyle(s).opacity) }; }`;

/** First frame time at which `pred` holds, and how many times `key` changed over the recording. */
function analyse(rec, pred, key) {
  const press = rec.marks.find((m) => m.type === "pointerdown")?.t ?? rec.marks[0]?.t ?? 0;
  const first = rec.frames.find((f) => f.t >= press && pred(f));
  let changes = 0;
  for (let i = 1; i < rec.frames.length; i++) if (rec.frames[i][key] !== rec.frames[i - 1][key]) changes++;
  return { pressAt: round(press, 1), firstAt: first ? round(first.t, 1) : null, afterPressMs: first ? round(first.t - press, 1) : null, changes };
}

function summariseOpacity(rec, press) {
  const at = (ms) => rec.frames.find((f) => f.t >= press + ms);
  return { at60: at(60)?.opacity, at150: at(150)?.opacity, at250: at(250)?.opacity, at380: at(380)?.opacity, minWhilePending: Math.min(...rec.frames.filter((f) => f.pending).map((f) => f.opacity)) };
}

async function rowHeartSelector(win, wantLiked) {
  // A visible row on the current list whose heart is in the wanted state; marks it for selectors.
  return js(win, `(() => {
    document.querySelectorAll("[data-probe-row]").forEach((el) => el.removeAttribute("data-probe-row"));
    const main = document.querySelector('[data-testid="main-scroll-region"]').getBoundingClientRect();
    const rows = [...document.querySelectorAll('[data-testid="virtual-track-row"]')].filter((row) => { const r = row.getBoundingClientRect(); return r.top > main.top + 120 && r.bottom < main.bottom - 140; });
    const row = rows.find((row) => { const h = row.querySelector('[data-testid="track-like-btn"]'); return h && (${wantLiked} === null || (h.getAttribute("aria-pressed") === "true") === ${wantLiked}); });
    if (!row) return null;
    row.setAttribute("data-probe-row", "");
    return { title: row.textContent.trim().slice(0, 40) };
  })()`);
}

const HEART = '[data-probe-row] [data-testid="track-like-btn"]';
const HEART_SAMPLER = `() => { const h = document.querySelector('${HEART}'); return h ? { pressed: h.getAttribute("aria-pressed"), pending: h.hasAttribute("data-pending"), opacity: Number(getComputedStyle(h.querySelector("svg")).opacity), filled: h.querySelector("svg").hasAttribute("data-filled") } : { pressed: "gone" }; }`;

export async function sectionFeedback(win, dir) {
  const out = { latency: { command: 400, mutation: 400 } };
  await navigate(win, "home");
  await setLatency(win, { command: 400 });
  for (const step of ["play", "pause"]) {
    await startRec(win, PLAY_SAMPLER);
    await realClick(win, '[data-testid="btn-play-pause"]');
    await sleep(1100);
    const rec = await stopRec(win);
    const target = step === "play" ? "pause" : "play";
    const press = rec.marks.find((m) => m.type === "pointerdown")?.t ?? 0;
    const settled = rec.frames.find((f) => f.t > press + 50 && !f.pending);
    out[step] = {
      iconShownAt: analyse(rec, (f) => f.icon === target, "icon"),
      iconChanges: analyse(rec, () => true, "icon").changes,
      finalIcon: rec.frames.at(-1).icon,
      pendingClearedAfterPressMs: settled ? round(settled.t - press, 1) : null,
      opacity: summariseOpacity(rec, press),
    };
  }

  // Contrast of the dimmed icon: held pending for 5 s, measured 700 ms after a real press.
  out.contrast = {};
  for (const theme of ["dark", "light"]) {
    await setAppearance(win, { theme, reduceTransparency: false });
    await openLongList(win, 2000);
    await setLatency(win, { command: 5000, mutation: 5000 });
    const c = (out.contrast[theme] = {});
    const measure = async (name, selector, press) => {
      const r = await box(win, selector);
      win.webContents.sendInputEvent({ type: "mouseMove", x: Math.round(r.x + r.w / 2), y: Math.round(r.y + r.h / 2) });
      await sleep(400);
      const before = await iconContrast(win, selector);
      await press();
      await sleep(700);
      const dimmed = await iconContrast(win, selector, join(dir, `dim-${theme}-${name}.png`));
      await sleep(5200);
      const settled = await iconContrast(win, selector);
      c[name] = { before, dimmed, settled };
    };
    await measure("play (accent)", '[data-testid="btn-play-pause"]', () => realClick(win, '[data-testid="btn-play-pause"]'));
    await measure("next (glass bar)", '[data-testid="btn-next"]', () => realClick(win, '[data-testid="btn-next"]'));
    await measure("like (glass bar)", '[data-testid="player-like"]', () => realClick(win, '[data-testid="player-like"]'));
    c.rowHeartRow = await rowHeartSelector(win, null);
    await measure("row heart", HEART, () => realClick(win, HEART));
    await setLatency(win, { command: 0, mutation: 0 });
    // Leave the player paused for the next steps.
    if ((await js(win, `document.querySelector('[data-testid="btn-play-pause"] svg').dataset.icon`)) === "pause") await realClick(win, '[data-testid="btn-play-pause"]');
    await sleep(300);
  }
  await setAppearance(win, { theme: "dark" });

  // The heart turns at the press and settles; a double click ends on the last click.
  await openLongList(win, 2000);
  await setLatency(win, { mutation: 400 });
  out.heart = {};
  out.heart.row = await rowHeartSelector(win, false);
  await startRec(win, HEART_SAMPLER);
  await realClick(win, HEART);
  await sleep(1200);
  let rec = await stopRec(win);
  out.heart.single = { turned: analyse(rec, (f) => f.pressed === "true", "pressed"), final: rec.frames.at(-1) };
  await sleep(300);
  await startRec(win, HEART_SAMPLER);
  await realClick(win, HEART);
  await sleep(90);
  await realClick(win, HEART, { clickCount: 2 });
  await sleep(1600);
  rec = await stopRec(win);
  out.heart.double = {
    note: "row was liked by the single click; two clicks 90 ms apart should end liked again",
    presses: rec.marks.filter((m) => m.type === "pointerdown").map((m) => round(m.t, 1)),
    pressedSequence: rec.frames.reduce((seq, f) => (seq.at(-1)?.pressed === f.pressed ? seq : [...seq, { t: round(f.t, 1), pressed: f.pressed }]), []),
    final: rec.frames.at(-1),
    fakeLiked: await js(win, `(() => { const id = globalThis.__probe.log.filter((l) => l.name === "sendMutation").at(-1)?.arg.videoId; return id ? globalThis.__probe.fake.likedVideoIds.has(id) : null; })()`),
    mutations: await js(win, `globalThis.__probe.log.filter((l) => l.name === "sendMutation").slice(-3).map((l) => ({ liked: l.arg.liked, videoId: l.arg.videoId }))`),
  };

  // The track menu while a like is pending.
  await setLatency(win, { mutation: 5000 });
  out.menu = { row: await rowHeartSelector(win, null) };
  const menuState = `(() => [...document.querySelectorAll('[data-testid="track-actions"] button')].map((b) => ({ id: b.dataset.testid ?? b.textContent.trim().slice(0, 20), disabled: b.disabled || b.getAttribute("aria-disabled") === "true" })))()`;
  await realClick(win, '[data-probe-row] [data-testid="track-actions-btn"]');
  await until(win, '__glassProbe.has("track-actions")');
  await sleep(500);
  out.menu.idle = await js(win, menuState);
  await key(win, "Escape");
  await sleep(500);
  await realClick(win, HEART);
  await sleep(200);
  await realClick(win, '[data-probe-row] [data-testid="track-actions-btn"]');
  await until(win, '__glassProbe.has("track-actions")');
  await sleep(400);
  out.menu.whileLikePending = await js(win, menuState);
  out.menu.heartPending = await js(win, `document.querySelector('${HEART}')?.hasAttribute("data-pending") ?? null`);
  await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "menu-like-pending.png"), img.resize({ width: 1280 }).toPNG()));
  // An enabled item still works while the like waits: Go to album, by a real click.
  await realClick(win, '[data-testid="action-go-album"]');
  await sleep(700);
  out.menu.goAlbumWhilePending = { albumOpened: await js(win, '__glassProbe.has("view-album")'), menuClosed: await js(win, '!__glassProbe.has("track-actions")') };
  await sleep(4600);

  // A rejected like turns back.
  await setLatency(win, { mutation: 400 });
  await setProbe(win, `{ rejectMutation: "setLike" }`);
  out.rejected = { row: await rowHeartSelector(win, null) };
  await startRec(win, HEART_SAMPLER);
  await realClick(win, HEART);
  await sleep(1500);
  rec = await stopRec(win);
  out.rejected.pressedSequence = rec.frames.reduce((seq, f) => (seq.at(-1)?.pressed === f.pressed ? seq : [...seq, { t: round(f.t, 1), pressed: f.pressed, pending: f.pending }]), []);
  out.rejected.press = rec.marks.find((m) => m.type === "pointerdown")?.t;
  out.rejected.toasts = await js(win, `[...document.querySelectorAll(".toast")].map((t) => t.textContent.trim().slice(0, 80))`);
  await setProbe(win, `{ rejectMutation: null }`);
  await setLatency(win, { mutation: 0, command: 0 });
  json(dir, "feedback", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 4. navigation
// ---------------------------------------------------------------------------------------------

const EXTRA_PLAYLISTS = [1, 2, 3, 4].map((n) => ({ playlistId: `probe-pl-${n}`, browseId: null, title: `Probe Playlist ${n}`, subtitle: "Probe-shaped", trackCount: 120 + n, cover: null }));

/** Page side: library gains four probe playlists; B and the probe playlists get their own rows. */
function shapeCatalog(extra) {
  const cfg = globalThis.__probe;
  const previous = cfg.shape.catalog;
  cfg.shape.catalog = (fake, req, res) => {
    if (req.kind === "libraryPlaylists" && res.resultType === "libraryPlaylists") return { ...res, items: [...res.items, ...extra] };
    if (req.kind === "playlistTracks" && res.resultType === "playlistTracks" && req.playlistId !== "mock-pl-favorites") {
      const known = extra.find((p) => p.playlistId === req.playlistId);
      const count = known?.trackCount ?? 42;
      const base = known ? 3000 + Number(req.playlistId.slice(-1)) * 400 : 5000;
      const offset = req.cursor ? Number(req.cursor) : 0;
      const items = fake.tracks10k.slice(base + offset, base + Math.min(count, offset + (req.limit ?? 100)));
      return { ...res, playlistId: req.playlistId, title: known?.title ?? "Late-Night City Glass Radio (Mock)", tracks: { ...res.tracks, items, nextCursor: offset + items.length < count ? String(offset + items.length) : null } };
    }
    return previous ? previous(fake, req, res) : res;
  };
  return true;
}

const PAGE_SAMPLER = `() => {
  const main = document.querySelector('[data-testid="main-scroll-region"]');
  const loading = document.querySelector('[data-testid$="-state-loading"]');
  // During a page transition both pages are in the document; the last one is the page arriving.
  const views = [...document.querySelectorAll('[data-testid^="view-"]')];
  const view = views.at(-1);
  const h1 = view?.querySelector("h1");
  return { view: view?.dataset.testid ?? null, views: views.length, h1: h1?.textContent.trim().slice(0, 40) ?? null, scrollTop: main.scrollTop,
    loading: loading ? Number(getComputedStyle(loading).opacity) : null, rows: document.querySelectorAll('[data-testid="virtual-track-row"]').length,
    firstRow: document.querySelector('[data-testid="virtual-track-row"]')?.textContent.trim().slice(0, 30) ?? null };
}`;

/** What a page showed from its first frame on: header, rows, the loading block's opacity over time. */
function pageTimeline(rec, viewId) {
  // Timed from the click or, for Back, the Escape keydown (the recorder starts 50 ms earlier).
  const click = rec.marks.find((m) => m.type === "click" || m.type === "keydown")?.t ?? 0;
  const frames = rec.frames.filter((f) => f.t >= click);
  const onView = frames.filter((f) => f.view === viewId);
  const first = onView[0];
  const loadingVisible = onView.find((f) => f.loading != null && f.loading > 0.05);
  const rowsAt = onView.find((f) => f.rows > 0);
  return {
    firstFrameMs: first ? round(first.t - click, 1) : null,
    firstFrame: first ? { h1: first.h1, rows: first.rows, loading: first.loading, scrollTop: first.scrollTop } : null,
    loadingVisibleAfterMs: loadingVisible ? round(loadingVisible.t - click, 1) : null,
    loadingMaxOpacity: Math.max(0, ...onView.map((f) => f.loading ?? 0)),
    rowsAfterMs: rowsAt ? round(rowsAt.t - click, 1) : null,
    h1Values: [...new Set(onView.map((f) => f.h1))],
    cls: round(rec.cls, 4), clsWithoutInput: round(rec.clsWithoutInput, 4), shifts: rec.shifts.slice(0, 8),
  };
}

async function recordOpen(win, clickExpr, viewId, ms) {
  await startRec(win, PAGE_SAMPLER);
  await sleep(50);
  await js(win, clickExpr);
  await sleep(ms);
  return pageTimeline(await stopRec(win), viewId);
}

const clickTestid = (id) => `(() => { document.querySelector('[data-testid="${id}"]').click(); return true; })()`;
const escapeKey = `(() => { document.activeElement?.blur(); document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); return true; })()`;

export async function sectionNav(win, dir) {
  const out = { shaped: ["library has four extra probe playlists; B and probe playlists return their own rows and titles"] };
  await call(win, shapeCatalog, EXTRA_PLAYLISTS);
  // Pages open at the top; Back returns to the old offset.
  win.setContentSize(1280, 600);
  await navigate(win, "home");
  await sleep(300);
  const homeMax = await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); m.scrollTop = 400; return { max: m.scrollHeight - m.clientHeight, top: m.scrollTop }; })()`);
  await sleep(300);
  out.home = homeMax;
  out.queue = await recordOpen(win, clickTestid("nav-queue"), "view-queue", 500);
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 150; return true; })()`);
  await sleep(200);
  out.queueScrolledTo = await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`);
  out.settings = await recordOpen(win, clickTestid("nav-settings"), "view-settings", 500);
  out.backToQueue = await recordOpen(win, escapeKey, "view-queue", 600);
  out.backToHome = await recordOpen(win, escapeKey, "view-home", 600);
  win.setContentSize(1280, 800);
  await sleep(300);

  // Collections with catalog latency.
  for (const ms of [300, 1500]) {
    const r = (out[`latency${ms}`] = {});
    await setLatency(win, { catalog: { playlistTracks: 0 } });
    await navigate(win, "library");
    await until(win, '__glassProbe.has("playlist-card-probe-pl-1")');
    await setLatency(win, { catalog: { playlistTracks: ms } });
    const fresh = ms === 300 ? "probe-pl-1" : "probe-pl-2";
    r.firstOpen = await recordOpen(win, clickTestid(`playlist-card-${fresh}`), "view-playlist", ms + 900);
    await js(win, escapeKey);
    await until(win, '__glassProbe.has("view-library")');
    await sleep(300);
    // A, then B, then Back to A (A through the library, B from the sidebar's library, then Back twice).
    r.openA = await recordOpen(win, clickTestid("playlist-card-mock-pl-favorites"), "view-playlist", ms + 600);
    await js(win, clickTestid("nav-library"));
    await until(win, '__glassProbe.has("view-library")');
    await sleep(300);
    r.openB = await recordOpen(win, clickTestid("playlist-card-mock-pl-late-night"), "view-playlist", ms + 600);
    await js(win, escapeKey);
    await until(win, '__glassProbe.has("view-library")');
    await sleep(200);
    r.backToA = await recordOpen(win, escapeKey, "view-playlist", ms + 600);
    await js(win, escapeKey);
    await until(win, '__glassProbe.has("view-library")');
    r.reopenA = await recordOpen(win, clickTestid("playlist-card-mock-pl-favorites"), "view-playlist", ms + 600);
    r.reopenB = (await js(win, escapeKey), await sleep(300), await recordOpen(win, clickTestid("playlist-card-mock-pl-late-night"), "view-playlist", ms + 600));
  }
  await setLatency(win, { catalog: {} });

  // A new search keeps the old results until the reply.
  await navigate(win, "search");
  const submit = (text) => `(() => { const i = document.querySelector('[data-testid="search-input"]'); i.value = ${JSON.stringify(text)}; i.dispatchEvent(new Event("input", { bubbles: true })); i.closest("form").requestSubmit(); return true; })()`;
  await js(win, submit("Neon"));
  await until(win, `document.querySelector('[data-testid="search-results"] [data-testid="virtual-track-row"]')`);
  await sleep(400);
  await setLatency(win, { catalog: { searchSongs: 1500 } });
  await startRec(win, `() => ({ busy: document.querySelector('[data-testid="search-results"]').getAttribute("aria-busy"), loading: !!document.querySelector('[data-testid="search-state-loading"]'), empty: !!document.querySelector('[data-testid="search-state-empty"]'), rows: document.querySelectorAll('[data-testid="search-results"] [data-testid="virtual-track-row"]').length, first: document.querySelector('[data-testid="search-results"] [data-testid="virtual-track-row"]')?.textContent.trim().slice(0, 24) ?? null })`);
  await sleep(50);
  await js(win, submit("夜"));
  await sleep(2200);
  const rec = await stopRec(win);
  const seq = rec.frames.reduce((s, f) => { const k = `${f.first}|${f.busy}|${f.loading}|${f.rows}`; return s.at(-1)?.k === k ? s : [...s, { k, t: round(f.t, 1), ...f }]; }, []).map(({ k, ...f }) => f);
  out.search = { sequence: seq, framesWithoutRows: rec.frames.filter((f) => f.rows === 0).length, loadingFrames: rec.frames.filter((f) => f.loading).length, cls: round(rec.cls, 4), clsWithoutInput: round(rec.clsWithoutInput, 4) };
  await setLatency(win, { catalog: {} });
  json(dir, "nav", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 5. after a write
// ---------------------------------------------------------------------------------------------

/** Page side: the account gains add-to-playlist; an add is confirmed and invalidates playlistTracks (no id, as core does today). */
function shapeAddToPlaylist() {
  const cfg = globalThis.__probe;
  const fake = cfg.fake;
  fake.snapshot.capabilities = { ...fake.snapshot.capabilities, playlistAddMutation: "supported" };
  fake.emit({ deltaType: "capabilitiesChanged", revision: ++fake.snapshot.revision, capabilities: structuredClone(fake.snapshot.capabilities) });
  cfg.shape.mutation = (f, req) => {
    if (req.kind !== "addToPlaylist") return null;
    f.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistTracks", revision: ++f.snapshot.revision });
    return { kind: req.kind, outcome: "confirmed", error: null };
  };
  return true;
}

const LIST_STATE = `(() => {
  const main = document.querySelector('[data-testid="main-scroll-region"]');
  const list = document.querySelector('[data-testid="virtual-track-list"]');
  const m = main.getBoundingClientRect();
  const at = document.elementFromPoint(m.left + m.width / 2, m.top + m.height / 2)?.closest('[data-testid="virtual-track-row"]');
  return { scrollTop: main.scrollTop, listHeight: list?.scrollHeight ?? null, rowAtCentre: at?.textContent.trim().slice(0, 36) ?? null,
    count: document.querySelector('[data-testid$="-count"]')?.textContent.trim() ?? null, loading: !!document.querySelector('[data-testid$="-state-loading"]'),
    tail: !!document.querySelector('[data-testid="track-list-tail"]') };
})()`;

export async function sectionWrite(win, dir) {
  const out = { shaped: ["add-to-playlist capability turned on via capabilitiesChanged; an add is confirmed and emits resourceInvalidated{resource_kind:'playlistTracks'} with no id (the shape the unit test uses for today's core)"] };
  await call(win, shapeCatalog, EXTRA_PLAYLISTS);
  await call(win, shapeAddToPlaylist);
  await navigate(win, "library");
  await js(win, clickTestid("playlist-card-mock-pl-favorites"));
  await until(win, `document.querySelector('[data-testid="virtual-track-row"]')`);
  for (let i = 0; i < 6; i++) {
    await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); m.scrollTop = m.scrollHeight; return true; })()`);
    await sleep(500);
  }
  out.pagesRead = await js(win, `globalThis.__probe.log.filter((l) => l.name === "queryCatalog" && l.arg.kind === "playlistTracks" && l.arg.playlistId === "mock-pl-favorites").map((l) => l.arg.cursor ?? null)`);
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 9000; return true; })()`);
  await sleep(500);
  // The re-read after the write answers 300 ms per page, so a list cut back and regrown would show.
  out.catalogLatencyMs = 300;
  await setLatency(win, { catalog: { playlistTracks: 300, likedSongs: 300 } });
  out.before = await js(win, LIST_STATE);
  await rowHeartSelector(win, null);
  await realClick(win, '[data-probe-row] [data-testid="track-actions-btn"]');
  await until(win, '__glassProbe.has("action-add-to-playlist")');
  await sleep(300);
  await js(win, clickTestid("action-add-to-playlist"));
  await until(win, '__glassProbe.has("pick-playlist-mock-pl-late-night")');
  await js(win, clickTestid("pick-playlist-mock-pl-late-night"));
  await sleep(100);
  const logFrom = await js(win, "globalThis.__probe.log.length");
  await startRec(win, `() => (${LIST_STATE.slice(1, -3)})()`);
  await js(win, clickTestid("confirm-add-to-playlist"));
  await sleep(4000);
  const rec = await stopRec(win);
  out.after = await js(win, LIST_STATE);
  out.changesDuring = rec.frames.reduce((s, f) => { const k = `${f.scrollTop}|${f.listHeight}|${f.rowAtCentre}|${f.loading}`; return s.at(-1)?.k === k ? s : [...s, { k, t: round(f.t, 1), ...f }]; }, []).map(({ k, ...f }) => f);
  out.requestsAfterAdd = await js(win, `globalThis.__probe.log.slice(${logFrom}).map((l) => ({ name: l.name, kind: l.arg?.kind, cursor: l.arg?.cursor ?? null, playlistId: l.arg?.playlistId }))`);
  out.cls = round(rec.cls, 4);
  out.toasts = await js(win, `[...document.querySelectorAll(".toast")].map((t) => t.textContent.trim().slice(0, 80))`);

  // The same add with an invalidation that names the playlist written to (the core's current
  // shape): an add to B (late night) while A (favorites) is open and deep reads nothing for A.
  await js(win, `(() => { globalThis.__probe.shape.mutation = (f, req) => { if (req.kind !== "addToPlaylist") return null; f.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistTracks", resource_id: req.playlistId, revision: ++f.snapshot.revision }); return { kind: req.kind, outcome: "confirmed", error: null }; }; return true; })()`);
  await sleep(600);
  const named = (out.namedOther = { shaped: "resourceInvalidated{resource_kind:'playlistTracks', resource_id: the add's playlistId}", before: await js(win, LIST_STATE) });
  await rowHeartSelector(win, null);
  await realClick(win, '[data-probe-row] [data-testid="track-actions-btn"]');
  await until(win, '__glassProbe.has("action-add-to-playlist")');
  await sleep(300);
  await js(win, clickTestid("action-add-to-playlist"));
  await until(win, '__glassProbe.has("pick-playlist-mock-pl-late-night")');
  await js(win, clickTestid("pick-playlist-mock-pl-late-night"));
  await sleep(100);
  const namedFrom = await js(win, "globalThis.__probe.log.length");
  await startRec(win, `() => (${LIST_STATE.slice(1, -3)})()`);
  await js(win, clickTestid("confirm-add-to-playlist"));
  await sleep(2500);
  const namedRec = await stopRec(win);
  named.after = await js(win, LIST_STATE);
  named.changesDuring = namedRec.frames.reduce((s, f) => { const k = `${f.scrollTop}|${f.listHeight}|${f.rowAtCentre}|${f.loading}`; return s.at(-1)?.k === k ? s : [...s, { k, t: round(f.t, 1), ...f }]; }, []).map(({ k, ...f }) => f);
  named.requestsAfterAdd = await js(win, `globalThis.__probe.log.slice(${namedFrom}).map((l) => ({ name: l.name, kind: l.arg?.kind, cursor: l.arg?.cursor ?? null, playlistId: l.arg?.playlistId }))`);
  named.catalogRequestsForA = named.requestsAfterAdd.filter((r) => r.name === "queryCatalog" && r.playlistId === "mock-pl-favorites").length;
  named.unchanged = named.before.scrollTop === named.after.scrollTop && named.before.listHeight === named.after.listHeight && named.before.rowAtCentre === named.after.rowAtCentre;
  named.cls = round(namedRec.cls, 4);
  await js(win, "(globalThis.__probe.shape.mutation = null, true)");

  // Unlike on Liked songs: the row stays, with an empty heart.
  await navigate(win, "liked");
  await until(win, `document.querySelector('[data-testid="virtual-track-row"]')`);
  await sleep(400);
  const liked = await rowHeartSelector(win, true);
  const rows = `[...document.querySelectorAll('[data-testid="virtual-track-row"]')].map((r) => r.textContent.trim().slice(0, 30))`;
  out.liked = { row: liked, before: await js(win, LIST_STATE), rowsBefore: await js(win, rows) };
  const likedLogFrom = await js(win, "globalThis.__probe.log.length");
  await startRec(win, `() => ({ rows: document.querySelectorAll('[data-testid="virtual-track-row"]').length, first: document.querySelector('[data-testid="virtual-track-row"]')?.textContent.trim().slice(0, 30), loading: !!document.querySelector('[data-testid$="-state-loading"]'), scrollTop: document.querySelector('[data-testid="main-scroll-region"]').scrollTop, listHeight: document.querySelector('[data-testid="virtual-track-list"]')?.scrollHeight ?? null })`);
  await realClick(win, HEART);
  await sleep(1800);
  const likedRec = await stopRec(win);
  out.liked.changesDuring = likedRec.frames.reduce((s, f) => { const k = `${f.rows}|${f.first}|${f.loading}|${f.scrollTop}|${f.listHeight}`; return s.at(-1)?.k === k ? s : [...s, { k, t: round(f.t, 1), ...f }]; }, []).map(({ k, ...f }) => f);
  out.liked.requests = await js(win, `globalThis.__probe.log.slice(${likedLogFrom}).map((l) => ({ name: l.name, kind: l.arg?.kind, cursor: l.arg?.cursor ?? null, t: Math.round(l.t) }))`);
  out.liked.after = await js(win, LIST_STATE);
  out.liked.rowsAfter = await js(win, rows);
  out.liked.heartAfter = await js(win, `(() => { const h = document.querySelector('${HEART}'); return h ? { pressed: h.getAttribute("aria-pressed"), filled: h.querySelector("svg").hasAttribute("data-filled") } : null; })()`);
  await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "liked-after-unlike.png"), img.resize({ width: 1280 }).toPNG()));
  await setLatency(win, { catalog: {} });
  json(dir, "write", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 6. Now Playing
// ---------------------------------------------------------------------------------------------

const NP_GEOMETRY = `() => {
  const r = (sel) => { const el = document.querySelector(sel); if (!el) return null; const b = el.getBoundingClientRect(); return [Math.round(b.x * 10) / 10, Math.round(b.y * 10) / 10, Math.round(b.width * 10) / 10, Math.round(b.height * 10) / 10]; };
  return { cover: r('[data-testid="np-cover-wrap"]'), title: r('[data-testid="now-playing-title"]'), barLike: r('[data-testid="player-like"]'), lyrics: r('.np-stage .np-lyrics'), upNext: r('[data-testid="np-up-next"]'),
    lyricsKind: document.querySelector('[data-testid="np-lyrics-hold"]') ? "hold" : document.querySelector('[data-testid="np-lyrics-preview"]') ? "preview" : "none",
    line: document.querySelector('[data-testid="np-up-next-line"]')?.textContent.trim().slice(0, 50) ?? null,
    titleText: document.querySelector('[data-testid="now-playing-title"]')?.textContent.trim().slice(0, 30) ?? null };
}`;

function geometryDrift(frames, keyName) {
  const rects = frames.map((f) => f[keyName]).filter(Boolean);
  if (rects.length === 0) return null;
  const first = rects[0];
  const drift = Math.max(...rects.map((r) => Math.max(...r.map((v, i) => Math.abs(v - first[i])))));
  const changes = [];
  for (let i = 1; i < frames.length; i++) if (frames[i][keyName] && frames[i - 1][keyName] && frames[i][keyName].join() !== frames[i - 1][keyName].join()) changes.push({ t: round(frames[i].t, 1), to: frames[i][keyName] });
  return { first, drift: round(drift, 1), distinct: new Set(rects.map((r) => r.join(","))).size, missingFrames: frames.length - rects.length, changes };
}

export async function sectionNowPlaying(win, dir) {
  const out = {};
  const upNextState = `(() => {
    const d = (sel) => { const el = document.querySelector(sel); return el ? getComputedStyle(el).display : null; };
    const main = document.querySelector('[data-testid="main-scroll-region"]');
    const rows = [...document.querySelectorAll('[data-testid="np-up-next-row"]')];
    return { window: [innerWidth, innerHeight], contentWidth: main.clientWidth, line: d('[data-testid="np-up-next-line"]'), list: d(".np-up-next-list"),
      upNextHeight: document.querySelector('[data-testid="np-up-next"]')?.getBoundingClientRect().height ?? null,
      lineText: document.querySelector('[data-testid="np-up-next-line"]')?.textContent.trim().slice(0, 60) ?? null,
      lineIsButton: !!document.querySelector('[data-testid="np-up-next-line-start"]'),
      openQueue: !!document.querySelector('[data-testid="np-up-next-open-queue"]'),
      rowsVisible: rows.filter((r) => getComputedStyle(r).display !== "none").length,
      rows: rows.map((r) => ({ suggestion: r.dataset.suggestion, button: !!r.querySelector("button"), focusable: !!r.querySelector('button, [tabindex], a[href]'), role: r.getAttribute("role"), shown: getComputedStyle(r).display !== "none" })) };
  })()`;
  // Short regular window: one line, nothing moves when it changes.
  win.setContentSize(1440, 800);
  await sleep(400);
  await navigate(win, "now-playing");
  await sleep(500);
  out.short = { size: win.getContentSize(), state: await js(win, upNextState) };
  await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "np-short-1440x800.png"), img.resize({ width: 1440 }).toPNG()));
  await setLatency(win, { lyrics: 900 });
  for (const op of ["next", "prev"]) {
    await startRec(win, NP_GEOMETRY);
    await sleep(60);
    await js(win, clickTestid(`btn-${op}`));
    await sleep(1800);
    const rec = await stopRec(win);
    out.short[op] = {
      cover: geometryDrift(rec.frames, "cover"), lyrics: geometryDrift(rec.frames, "lyrics"), upNext: geometryDrift(rec.frames, "upNext"), title: geometryDrift(rec.frames, "title"),
      lyricsKinds: rec.frames.reduce((s, f) => (s.at(-1)?.kind === f.lyricsKind ? s : [...s, { t: round(f.t, 1), kind: f.lyricsKind }]), []),
      lines: [...new Set(rec.frames.map((f) => f.line))], titles: [...new Set(rec.frames.map((f) => f.titleText))], cls: round(rec.cls, 4),
    };
  }
  // The same with the queue moving as core reports it (probe-shaped), between two one-line
  // titles, so only the Up next line changes: entry 2 to entry 3 of the shaped queue.
  out.shaped = ["queue replaced by 14 entries for the 'nextShapedQueue' case; next moves the current entry"];
  await call(win, shapeQueue);
  await js(win, "(globalThis.__probe.queueNext(), globalThis.__probe.queueNext(), true)");
  await sleep(1500);
  await startRec(win, NP_GEOMETRY);
  await sleep(60);
  await js(win, clickTestid("btn-next"));
  await sleep(1800);
  const shapedRec = await stopRec(win);
  out.short.nextShapedQueue = {
    cover: geometryDrift(shapedRec.frames, "cover"), lyrics: geometryDrift(shapedRec.frames, "lyrics"), upNext: geometryDrift(shapedRec.frames, "upNext"), title: geometryDrift(shapedRec.frames, "title"),
    lyricsKinds: shapedRec.frames.reduce((s, f) => (s.at(-1)?.kind === f.lyricsKind ? s : [...s, { t: round(f.t, 1), kind: f.lyricsKind }]), []),
    lines: [...new Set(shapedRec.frames.map((f) => f.line))], titles: [...new Set(shapedRec.frames.map((f) => f.titleText))], cls: round(shapedRec.cls, 4), shifts: shapedRec.shifts,
  };
  await setLatency(win, { lyrics: 0 });

  // Tall regular window: rows; queued rows are buttons, suggestions are not; Tab order and focus ring.
  win.setContentSize(1440, 1000);
  await sleep(500);
  out.tall = { size: win.getContentSize(), state: await js(win, upNextState) };
  await js(win, `(() => { document.querySelector('[data-testid="np-up-next-open-queue"]').focus(); return true; })()`);
  const focusInfo = `(() => { const el = document.activeElement; const cs = getComputedStyle(el); return { testid: el.dataset.testid ?? el.tagName, text: el.textContent.trim().slice(0, 30), focusVisible: el.matches(":focus-visible"), outline: cs.outlineStyle + " " + cs.outlineWidth + " " + cs.outlineColor, boxShadow: cs.boxShadow.slice(0, 80), inUpNext: !!el.closest('[data-testid="np-up-next"]') }; })()`;
  // Shift+Tab then Tab, so the heading is reached by keyboard and shows its ring.
  await key(win, "Tab", ["shift"]);
  await key(win, "Tab");
  out.tall.tabOrder = [await js(win, focusInfo)];
  for (let i = 0; i < 4; i++) { await key(win, "Tab"); out.tall.tabOrder.push(await js(win, focusInfo)); }
  await key(win, "Tab", ["shift"]); await key(win, "Tab", ["shift"]); await key(win, "Tab", ["shift"]);
  await key(win, "Tab");
  const up = await box(win, '[data-testid="np-up-next"]');
  if (up) await win.webContents.capturePage({ x: Math.max(0, Math.floor(up.x) - 8), y: Math.max(0, Math.floor(up.y) - 8), width: Math.ceil(up.w) + 16, height: Math.ceil(up.h) + 16 }).then((img) => writeFileSync(join(dir, "np-up-next-focus.png"), img.toPNG()));
  out.tall.focusForShot = await js(win, focusInfo);
  const logFrom = await js(win, "globalThis.__probe.log.length");
  await realClick(win, '[data-testid="np-up-next-row"][data-suggestion="false"] [data-testid="np-up-next-start"]');
  await sleep(600);
  out.tall.startCommand = await js(win, `globalThis.__probe.log.slice(${logFrom}).filter((l) => l.name === "sendPlayerCommand").map((l) => ({ op: l.arg.op, args: l.arg.args }))`);
  const sugg = await box(win, '[data-testid="np-up-next-row"][data-suggestion="true"]');
  if (sugg) {
    const from = await js(win, "globalThis.__probe.log.length");
    await realClick(win, '[data-testid="np-up-next-row"][data-suggestion="true"]');
    await sleep(400);
    out.tall.suggestionClickCommands = await js(win, `globalThis.__probe.log.slice(${from}).filter((l) => l.name === "sendPlayerCommand").map((l) => l.arg.op)`);
  }

  // Ultrawide content (2100 px and up) on a short window: the full column, not the one line.
  // The one-line rule is height-gated, so the short height is what makes this case discriminate.
  win.setContentSize(2600, 800);
  await sleep(600);
  const size = win.getContentSize();
  out.wide = { size, state: await js(win, upNextState) };
  if (size[0] >= 2400) await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "np-wide-2600x800.png"), img.resize({ width: 1300 }).toPNG()));
  // A short wide-tier window (content under 2100): the one line.
  win.setContentSize(1920, 800);
  await sleep(600);
  out.wideTierShort = { size: win.getContentSize(), state: await js(win, upNextState) };
  win.setContentSize(1280, 800);
  json(dir, "nowplaying", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 7. search
// ---------------------------------------------------------------------------------------------

/** Page side: search replies page 30 songs at a time and carry artists, albums and playlists. */
function shapeSearch() {
  const cfg = globalThis.__probe;
  const previous = cfg.shape.catalog;
  cfg.shape.catalog = (fake, req, res) => {
    if (req.kind !== "searchSongs" || res.resultType !== "searchSongs") return previous ? previous(fake, req, res) : res;
    const q = (req.query ?? "").trim().toLowerCase();
    const all = fake.tracks10k.filter((t) => t.title.toLowerCase().includes(q));
    const offset = req.cursor ? Number(req.cursor) : 0;
    const items = all.slice(offset, offset + 30);
    const next = offset + items.length < Math.min(all.length, 150) ? String(offset + items.length) : null;
    const first = all.slice(0, 12);
    return {
      ...res, songs: { ...res.songs, items, nextCursor: next },
      artists: [...new Map(first.map((t) => [t.artists[0].id, t.artists[0]])).values()].slice(0, 4),
      albums: [...new Map(first.map((t) => [t.album.id, t.album])).values()].slice(0, 4),
      playlists: fake.playlists,
    };
  };
  return true;
}

export async function sectionSearch(win, dir) {
  const out = { shaped: ["searchSongs pages 30 at a time up to 150 and returns 4 artists, 4 albums and the 2 mock playlists"] };
  await call(win, shapeSearch);
  await navigate(win, "search");
  await js(win, `(() => { const i = document.querySelector('[data-testid="search-input"]'); i.value = "Neon"; i.dispatchEvent(new Event("input", { bubbles: true })); i.closest("form").requestSubmit(); return true; })()`);
  await until(win, '__glassProbe.has("search-all-songs")');
  await sleep(500);
  out.order = await js(win, `(() => {
    const root = document.querySelector('[data-testid="search-results"]');
    return [...root.querySelectorAll('h2, [data-testid="search-all-songs"]')].filter((el) => el.checkVisibility()).map((el) => el.dataset.testid ?? el.textContent.trim());
  })()`);
  out.songRows = await js(win, `document.querySelectorAll('[data-testid="search-songs"] [data-testid="virtual-track-row"]').length`);
  await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "search-grouped.png"), img.resize({ width: 1280 }).toPNG()));
  // Scrolled so "All songs" (the last control) is in view, as a user reaching it would be.
  out.scrolledTo = await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); document.querySelector('[data-testid="search-all-songs"]').scrollIntoView({ block: "center" }); return { top: m.scrollTop, max: m.scrollHeight - m.clientHeight }; })()`);
  await sleep(300);
  await realClick(win, '[data-testid="search-all-songs"]');
  await until(win, '__glassProbe.has("search-all-songs-list")');
  await sleep(400);
  out.allSongs = { scrollTop: await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`), focus: await js(win, "document.activeElement.dataset.testid ?? null"), rows: await js(win, `document.querySelector('[data-testid="search-all-songs-list"] [data-testid="virtual-track-list"]').scrollHeight`) };
  for (let i = 0; i < 5; i++) {
    await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); m.scrollTop = m.scrollHeight; return true; })()`);
    await sleep(500);
  }
  out.allSongs.cursorsRead = await js(win, `globalThis.__probe.log.filter((l) => l.name === "queryCatalog" && l.arg.kind === "searchSongs").map((l) => l.arg.cursor ?? null)`);
  out.allSongs.listHeightAfterPaging = await js(win, `document.querySelector('[data-testid="search-all-songs-list"] [data-testid="virtual-track-list"]').scrollHeight`);
  // The back control heads the full list: scroll up to it, then press it.
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 0; return true; })()`);
  await sleep(400);
  await realClick(win, '[data-testid="search-songs-back"]');
  await sleep(500);
  out.back = {
    grouped: await js(win, '__glassProbe.has("search-songs") && !__glassProbe.has("search-all-songs-list")'), scrollTop: await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`), focus: await js(win, "document.activeElement.dataset.testid ?? document.activeElement.tagName"), focusVisible: await js(win, "document.activeElement.matches(':focus-visible')") };
  // From a mid-page offset (All songs pressed by a DOM click, as from the keyboard).
  out.midOffset = { set: await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); m.scrollTop = 300; return m.scrollTop; })()`) };
  await sleep(300);
  await js(win, clickTestid("search-all-songs"));
  await until(win, '__glassProbe.has("search-all-songs-list")');
  await sleep(400);
  out.midOffset.allSongsScrollTop = await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`);
  await js(win, clickTestid("search-songs-back"));
  await sleep(500);
  out.midOffset.back = { grouped: await js(win, '__glassProbe.has("search-songs") && !__glassProbe.has("search-all-songs-list")'), scrollTop: await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`), focus: await js(win, "document.activeElement.dataset.testid ?? document.activeElement.tagName") };
  json(dir, "search", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 8. lyrics
// ---------------------------------------------------------------------------------------------

/** Page side: 60 synced lines, one every 3 s from 1 s. */
function shapeLyrics() {
  globalThis.__probe.shape.lyrics = (_fake, req, res) => ({ ...res, syncedLines: Array.from({ length: 60 }, (_, i) => ({ timeMs: 1000 + i * 3000, text: `Probe line ${i + 1} · ${String(Math.floor((1 + i * 3) / 60)).padStart(1, "0")}:${String((1 + i * 3) % 60).padStart(2, "0")}` })) });
  return true;
}

const LYRICS_SAMPLER = `() => {
  const list = document.querySelector('[data-testid="lyrics-synced-list"]');
  if (!list) return { list: false };
  const cur = list.querySelector('[aria-current="true"]');
  const l = list.getBoundingClientRect(), c = cur?.getBoundingClientRect();
  return { list: true, scrollTop: Math.round(list.scrollTop * 10) / 10, current: cur ? Number(cur.dataset.lineIndex) : null,
    currentCentreOffset: c ? Math.round((c.top + c.height / 2) - (l.top + l.height / 2)) : null, currentInView: c ? c.top >= l.top && c.bottom <= l.bottom : null,
    seek: Number(document.querySelector('[data-testid="player-seek-slider"]').value), time: document.querySelector('[data-testid="player-pos-text"]')?.textContent.trim() };
}`;

const seekFake = (win, positionMs, op) => js(win, `(() => { const f = globalThis.__probe.fake; ${op ? `f.sendPlayerCommand({ commandId: "probe-${op}-" + Date.now(), op: "${op}" });` : ""} f.sendPlayerCommand({ commandId: "probe-seek-" + Date.now(), op: "seek", args: { positionMs: ${positionMs} } }); return true; })()`);

export async function sectionLyrics(win, dir) {
  const out = { shaped: ["lyrics have 60 synced lines (every 3 s) instead of the fixture's 5"] };
  await call(win, shapeLyrics);
  await navigate(win, "home");
  // A track never played before, so its lyrics are fetched (and shaped) fresh.
  await js(win, `(() => { const f = globalThis.__probe.fake; const p = f.snapshot.player; p.currentTrack = f.tracks10k[50]; p.trackInstanceId = "probe-ti-lyrics"; p.timeAnchor = { ...p.timeAnchor, positionMs: 0, durationMs: 240000 }; f.emit({ deltaType: "playerChanged", player: structuredClone(p), pending: structuredClone(f.snapshot.pending), revision: ++f.snapshot.revision }); return true; })()`);
  await sleep(600);
  for (const [name, positionMs, op] of [["playing", 120_000, "play"], ["paused", 60_000, "pause"]]) {
    await seekFake(win, positionMs, op);
    await navigate(win, "home");
    await sleep(500);
    await startRec(win, LYRICS_SAMPLER);
    await sleep(40);
    await js(win, clickTestid("nav-lyrics"));
    await sleep(900);
    const rec = await stopRec(win);
    const click = rec.marks.find((m) => m.type === "click")?.t ?? 0;
    const shown = rec.frames.filter((f) => f.t >= click && f.list);
    const first = shown[0];
    out[name] = {
      firstListFrameMs: first ? round(first.t - click, 1) : null,
      firstFrame: first ?? null,
      scrollTopFirst300ms: shown.filter((f) => f.t - click <= 300).map((f) => [round(f.t - click, 0), f.scrollTop]),
      final: shown.at(-1) ?? null,
      distinctScrollTops: [...new Set(shown.map((f) => f.scrollTop))].length,
    };
  }
  await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "lyrics-paused.png"), img.resize({ width: 1280 }).toPNG()));
  // Click a line four below the current one: highlight, seek bar and time together.
  const target = await js(win, `(() => { const cur = document.querySelector('[data-testid="lyrics-synced-list"] [aria-current="true"]'); const i = Number(cur?.dataset.lineIndex ?? 0) + 3; document.querySelectorAll("[data-probe-line]").forEach((e) => e.removeAttribute("data-probe-line")); const el = document.querySelector('[data-testid="lyrics-synced-list"] [data-line-index="' + i + '"]'); el?.setAttribute("data-probe-line", ""); return i; })()`);
  await setLatency(win, { command: 300 });
  await startRec(win, LYRICS_SAMPLER);
  await sleep(40);
  await realClick(win, "[data-probe-line]");
  await sleep(1000);
  const rec = await stopRec(win);
  await setLatency(win, { command: 0 });
  const press = rec.marks.find((m) => m.type === "pointerdown")?.t ?? 0;
  const seq = rec.frames.filter((f) => f.t >= press - 20).reduce((s, f) => { const k = `${f.current}|${f.seek}|${f.time}`; return s.at(-1)?.k === k ? s : [...s, { k, t: round(f.t - press, 1), current: f.current, seek: f.seek, time: f.time }]; }, []).map(({ k, ...f }) => f);
  out.clickLine = { target, latencyMs: 300, sequence: seq };
  json(dir, "lyrics", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 9. lists: queue focus, reduced motion
// ---------------------------------------------------------------------------------------------

/** Page side: a 14-entry queue; a next or previous moves along it as core would report. */
function shapeQueue() {
  const cfg = globalThis.__probe;
  const fake = cfg.fake;
  const tracks = fake.tracks10k.slice(100, 114);
  const entries = tracks.map((track, i) => ({ entryId: `probe-qe-${i}`, videoId: track.videoId, track, availability: "available" }));
  let at = 0;
  const publish = () => {
    const s = fake.snapshot;
    s.player.currentTrack = entries[at].track;
    s.player.currentQueueEntryId = entries[at].entryId;
    s.player.trackInstanceId = `probe-ti-${Date.now()}-${at}`;
    s.player.timeAnchor = { ...s.player.timeAnchor, positionMs: 0, durationMs: entries[at].track.durationMs ?? 200000 };
    s.queueSummary = { ...s.queueSummary, queueRevision: s.queueSummary.queueRevision + 1, currentEntryId: entries[at].entryId, previewEntries: entries, previewStart: 0, loadedCount: entries.length, completeness: "complete", hasMore: false };
    fake.emit({ deltaType: "queueReplaced", queue_summary: structuredClone(s.queueSummary), revision: ++s.revision });
    fake.emit({ deltaType: "playerChanged", player: structuredClone(s.player), pending: structuredClone(s.pending), revision: ++s.revision });
  };
  cfg.shape.afterCommand = (_f, cmd) => {
    if (cmd.op === "next" && at < entries.length - 1) { at++; publish(); }
    if (cmd.op === "previous" && at > 0) { at--; publish(); }
    return null;
  };
  cfg.queueNext = () => fake.sendPlayerCommand({ commandId: `probe-next-${Date.now()}-${Math.random()}`, op: "next" });
  publish();
  return true;
}

export async function sectionLists(win, dir) {
  const out = { shaped: ["queue replaced by 14 entries; next/previous move the current entry and publish queueReplaced + playerChanged"] };
  await call(win, shapeQueue);
  await navigate(win, "queue");
  await sleep(500);
  const focused = `(() => { const el = document.activeElement; const row = el.closest('[data-testid="virtual-track-row"]'); return { testid: el.dataset.testid ?? el.tagName, row: row?.textContent.trim().slice(0, 40) ?? null, inUpNext: !!el.closest('[data-testid="queue-up-next"]'), rowIndex: el.dataset.rowIndex ?? null, rowKey: row?.dataset.rowKey ?? null }; })()`;
  out.rowsInUpNext = await js(win, `document.querySelectorAll('[data-testid="queue-up-next"] [data-testid="virtual-track-row"]').length`);
  // Focus the sixth Up next row with the keyboard: focus the first, then ArrowDown five times.
  await js(win, `(() => { const r = document.querySelector('[data-testid="queue-up-next"] [data-testid="virtual-track-row"]'); (r.matches("[tabindex]") ? r : r.querySelector("[tabindex]") ?? r).focus(); return true; })()`);
  for (let i = 0; i < 5; i++) await key(win, "Down");
  out.focusedBefore = await js(win, focused);
  out.afterChanges = [];
  for (let i = 0; i < 3; i++) {
    await js(win, "(globalThis.__probe.queueNext(), true)");
    await sleep(500);
    out.afterChanges.push(await js(win, focused));
  }
  await key(win, "Down");
  out.afterArrowDown = await js(win, focused);
  out.upNextTitles = await js(win, `[...document.querySelectorAll('[data-testid="queue-up-next"] [data-testid="virtual-track-row"]')].map((r) => r.textContent.trim().slice(0, 40))`);

  // Reduced motion (the app setting).
  await setAppearance(win, { reduceMotion: true });
  const styleOf = (sel) => `(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) return null; const cs = getComputedStyle(el); return { transitionProperty: cs.transitionProperty, transitionDuration: cs.transitionDuration, transform: cs.transform, animation: cs.animationName + " " + cs.animationDuration }; })()`;
  out.reduceMotion = { root: await js(win, "__glassProbe.root()") };
  out.reduceMotion.bufferRing = await js(win, styleOf(".player-buffer-ring"));
  out.reduceMotion.scrubFlow = await js(win, styleOf(".player-seek .scrub-flow"));
  // Buffering, held: the ring and flow appear after 600 ms; sample their opacity and transform.
  const buffering = (on) => js(win, `(() => { const f = globalThis.__probe.fake; f.snapshot.player.playback = ${on ? '"buffering"' : '"paused"'}; f.emit({ deltaType: "playerChanged", player: structuredClone(f.snapshot.player), pending: structuredClone(f.snapshot.pending), revision: ++f.snapshot.revision }); return true; })()`);
  await startRec(win, `() => { const g = document.querySelector(".player-buffer-ring"), fl = document.querySelector(".player-seek .scrub-flow"); const a = getComputedStyle(g), b = getComputedStyle(fl); return { ring: Number(a.opacity), ringTransform: a.transform, ringAnim: a.animationName, flow: Number(b.opacity), flowTransform: b.transform, flowBefore: getComputedStyle(fl, "::before").transform }; }`);
  await buffering(true);
  await sleep(1100);
  await buffering(false);
  await sleep(500);
  let rec = await stopRec(win);
  // Runs of frames in which `k` changes, split where it rests for more than 50 ms: one per fade.
  const fade = (frames, k) => {
    const segments = [];
    for (let i = 1; i < frames.length; i++) {
      if (frames[i][k] === frames[i - 1][k]) continue;
      const last = segments.at(-1);
      if (last && frames[i].t - last.end <= 50) { last.end = frames[i].t; last.frames++; last.to = frames[i][k]; }
      else segments.push({ start: frames[i - 1].t, end: frames[i].t, frames: 1, from: frames[i - 1][k], to: frames[i][k] });
    }
    return segments.map((g) => ({ startMs: round(g.start, 0), spanMs: round(g.end - g.start, 0), frames: g.frames, from: g.from, to: g.to }));
  };
  out.reduceMotion.bufferingLive = { ring: fade(rec.frames, "ring"), flow: fade(rec.frames, "flow"), ringTransforms: [...new Set(rec.frames.map((f) => f.ringTransform))], ringAnimations: [...new Set(rec.frames.map((f) => f.ringAnim))], flowTransforms: [...new Set(rec.frames.map((f) => f.flowTransform))], flowBefore: fade(rec.frames, "flowBefore") };
  // Collection strip: shown once the page scrolls past the header.
  await navigate(win, "library");
  await js(win, clickTestid("playlist-card-mock-pl-favorites"));
  await until(win, `document.querySelector('[data-testid="virtual-track-row"]')`);
  await sleep(500);
  out.reduceMotion.strip = await js(win, styleOf(".collection-strip"));
  await startRec(win, `() => { const s = document.querySelector(".collection-strip"); const cs = getComputedStyle(s); return { o: Number(cs.opacity), tr: cs.transform, shown: s.classList.contains("is-shown") }; }`);
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 900; return true; })()`);
  await sleep(600);
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 0; return true; })()`);
  await sleep(600);
  rec = await stopRec(win);
  out.reduceMotion.stripLive = { opacity: fade(rec.frames, "o"), transforms: [...new Set(rec.frames.map((f) => f.tr))], shownStates: [...new Set(rec.frames.map((f) => f.shown))] };
  // Now Playing lines on a track change.
  await navigate(win, "now-playing");
  await sleep(500);
  out.reduceMotion.npLine = await js(win, styleOf(".np-line"));
  await startRec(win, `() => { const ls = [...document.querySelectorAll(".np-line")]; return { n: ls.length, o: ls.map((l) => Math.round(Number(getComputedStyle(l).opacity) * 100) / 100).join(","), tr: [...new Set(ls.map((l) => getComputedStyle(l).transform))].join("|"), y: ls.map((l) => Math.round(l.getBoundingClientRect().y)).join(",") }; }`);
  await seekFake(win, 9000, null);
  await sleep(700);
  rec = await stopRec(win);
  out.reduceMotion.npLineLive = { opacity: fade(rec.frames, "o"), transforms: [...new Set(rec.frames.map((f) => f.tr))], yValues: [...new Set(rec.frames.map((f) => f.y))].slice(0, 6) };
  await setAppearance(win, { reduceMotion: false });
  json(dir, "lists", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// 10. seek bar
// ---------------------------------------------------------------------------------------------

export async function sectionSeek(win, dir) {
  win.setContentSize(1440, 900);
  await sleep(400);
  await navigate(win, "home");
  // The mock never extrapolates; core does while playing, so the probe sets it after play.
  await js(win, `(() => { const f = globalThis.__probe.fake; f.sendPlayerCommand({ commandId: "probe-play-" + Date.now(), op: "play" }); const p = f.snapshot.player; p.timeAnchor = { ...p.timeAnchor, extrapolating: true, durationMs: 240000 }; f.emit({ deltaType: "playerChanged", player: structuredClone(p), pending: structuredClone(f.snapshot.pending), revision: ++f.snapshot.revision }); return true; })()`);
  await sleep(800);
  await startRec(win, `() => { const el = document.querySelector(".player-seek .scrub-fill"); return { w: el.getBoundingClientRect().width, track: el.parentElement.getBoundingClientRect().width, time: document.querySelector('[data-testid="player-pos-text"]').textContent.trim() }; }`);
  await sleep(3000);
  const rec = await stopRec(win);
  await js(win, `(() => { const f = globalThis.__probe.fake; f.sendPlayerCommand({ commandId: "probe-pause-" + Date.now(), op: "pause" }); return true; })()`);
  const changes = [];
  for (let i = 1; i < rec.frames.length; i++) if (rec.frames[i].w !== rec.frames[i - 1].w) changes.push({ t: rec.frames[i].t, dw: rec.frames[i].w - rec.frames[i - 1].w });
  const gaps = changes.slice(1).map((c, i) => c.t - changes[i].t).sort((a, b) => a - b);
  const steps = changes.map((c) => Math.abs(c.dw)).sort((a, b) => a - b);
  const intervals = rec.frames.slice(1).map((f, i) => f.t - rec.frames[i].t).sort((a, b) => a - b);
  const out = {
    shaped: ["timeAnchor.extrapolating set to true after play (the mock leaves it false); duration 240 s"],
    window: win.getContentSize(), trackWidth: rec.frames[0]?.track, frames: rec.frames.length,
    frameIntervalMs: { median: round(intervals[Math.floor(intervals.length / 2)], 2), p99: round(intervals[Math.floor(intervals.length * 0.99)], 2), max: round(intervals.at(-1), 2) },
    widthChanges: changes.length, gapBetweenChangesMs: { median: round(gaps[Math.floor(gaps.length / 2)], 1), max: round(gaps.at(-1), 1) },
    stepPx: { median: round(steps[Math.floor(steps.length / 2)], 3), max: round(steps.at(-1), 3) },
    netPx: round(rec.frames.at(-1).w - rec.frames[0].w, 2), timeText: [rec.frames[0].time, rec.frames.at(-1).time],
    negativeSteps: changes.filter((c) => c.dw < 0).length,
  };
  win.setContentSize(1280, 800);
  json(dir, "seek", out);
  return out;
}

// ---------------------------------------------------------------------------------------------
// second pass: covers, title block, menu and scrolling, preparing, recent searches, problems,
// paint times, leaving a re-read, Back to a reloaded Library
// ---------------------------------------------------------------------------------------------

/** Page side: publishes the player as core would, after `mutate(player, fake)`. */
const publishPlayer = (win, mutateSource) => js(win, `(() => { const f = globalThis.__probe.fake; const p = f.snapshot.player; (${mutateSource})(p, f); f.emit({ deltaType: "playerChanged", player: structuredClone(p), pending: structuredClone(f.snapshot.pending), revision: ++f.snapshot.revision }); return true; })()`);

/**
 * One 64 px PNG per colour for app://covers/<id>. Mock mode runs no daemon, so the shell answers
 * 404 for covers; the probe re-handles the app scheme on the UI session: covers from these PNGs,
 * ui files from ui/dist with the CSP header the shell's own handler sends (read first).
 */
async function serveCovers(win, colours) {
  const ses = win.webContents.session;
  const png = (rgb) => {
    const buf = Buffer.alloc(64 * 64 * 4);
    for (let i = 0; i < 64 * 64; i++) { buf[i * 4] = rgb[2]; buf[i * 4 + 1] = rgb[1]; buf[i * 4 + 2] = rgb[0]; buf[i * 4 + 3] = 255; }
    return nativeImage.createFromBitmap(buf, { width: 64, height: 64 }).toPNG();
  };
  const bodies = Object.fromEntries(Object.entries(colours).map(([id, rgb]) => [id, png(rgb)]));
  const served = [];
  const csp = (await ses.fetch("app://ui/index.html")).headers.get("content-security-policy");
  const dist = resolve(import.meta.dirname, "../../../ui/dist");
  const types = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
  ses.protocol.unhandle("app");
  ses.protocol.handle("app", async (request) => {
    const url = new URL(request.url);
    if (url.hostname === "covers") {
      const id = url.pathname.replace(/^\/+/, "");
      served.push(id);
      return bodies[id] ? new Response(bodies[id], { headers: { "Content-Type": "image/png" } }) : new Response("Not Found", { status: 404 });
    }
    if (url.hostname === "ui") {
      const rel = decodeURIComponent(url.pathname).replace(/^\/+/, "") || "index.html";
      const file = resolve(dist, rel);
      if (!file.startsWith(dist) || !existsSync(file)) return new Response("Not Found", { status: 404 });
      const ext = extname(file);
      return new Response(readFileSync(file), { headers: { "Content-Type": types[ext] ?? "application/octet-stream", ...(csp ? { "Content-Security-Policy": csp } : {}), "X-Content-Type-Options": "nosniff" } });
    }
    return new Response("Not Found", { status: 404 });
  });
  return served;
}

/** Leaves mock artwork: the client takes a fresh snapshot (forced by a revision gap) with mockMode false. */
async function realArtwork(win) {
  await js(win, `(() => { const f = globalThis.__probe.fake; f.snapshot.mockMode = false; f.simulateRevisionGapOnNextDelta(); f.emit({ deltaType: "playerChanged", player: structuredClone(f.snapshot.player), pending: structuredClone(f.snapshot.pending), revision: f.snapshot.revision += 2 }); return true; })()`);
  await sleep(800);
}

const COVER_SAMPLER = `() => {
  const id = (el) => { if (!el) return "none"; if (el.matches?.('[data-testid="cover-placeholder"]')) return "placeholder"; const m = (el.currentSrc || el.src || "").match(/covers\\/([\\w-]+)/); return m ? m[1] : (el.src?.startsWith("data:") ? "synthetic" : "?"); };
  const coverIn = (sel) => { const host = document.querySelector(sel); return host ? id(host.querySelector('img, [data-testid="cover-placeholder"]')) : "absent"; };
  const npCover = document.querySelector('[data-testid="np-cover-wrap"]');
  const npHead = document.querySelector(".np-head");
  const ambient = [...document.querySelectorAll('[data-testid="ambient-light"] .ambient-layer:not([data-leaving]) img')].map((i) => id(i));
  return {
    barTitle: document.querySelector(".player-track-title")?.textContent.trim().slice(0, 24) ?? null,
    bar: id(document.querySelector('[data-testid="player-bar"] .player-cover')),
    lyricsHead: id(document.querySelector(".lyrics-head-cover")),
    np: npCover ? coverIn('[data-testid="np-cover-wrap"]') : "absent",
    npOpacity: npCover ? Number(getComputedStyle(npCover).opacity) : null,
    npTitle: document.querySelector('[data-testid="now-playing-title"]')?.textContent.trim().slice(0, 24) ?? null,
    npTitleOpacity: npHead ? Number(getComputedStyle(npHead).opacity) : null,
    ambient: ambient.join(","),
  };
}`;

const COVER_TRACKS = `(f) => {
  const t = (i, tag, cover) => ({ ...f.tracks10k[i], title: "Probe track " + tag, cover });
  return {
    A: t(200, "A", { assetId: "probeA", remoteUrl: "https://example.invalid/a.jpg" }),
    B: t(201, "B", { assetId: null, remoteUrl: "https://example.invalid/b.jpg" }),
    C: t(202, "C", { assetId: null, remoteUrl: "https://example.invalid/c.jpg" }),
    N: t(203, "N", { assetId: null, remoteUrl: "https://example.invalid/n.jpg" }),
    P: t(204, "P", null),
  };
}`;

const setTrack = (win, tag, assetId) => publishPlayer(win, `(p, f) => { const tr = (${COVER_TRACKS})(f)["${tag}"]; ${assetId ? `tr.cover = { ...tr.cover, assetId: "${assetId}" };` : ""} p.currentTrack = tr; p.trackInstanceId = "probe-ti-${tag}-" + ${assetId ? `"asset"` : "Date.now()"}; p.timeAnchor = { ...p.timeAnchor, positionMs: 0, durationMs: 200000 }; }`);
// The cached cover arriving for the same track instance (core adds the asset a moment later).
const coverArrives = (win, tag, assetId) => publishPlayer(win, `(p) => { p.currentTrack = { ...p.currentTrack, cover: { ...p.currentTrack.cover, assetId: "${assetId}" } }; }`);

/** Frames where a surface shows `cover` while the shown title belongs to `tag`. */
function coverUnderTitle(frames, cover, tag) {
  const wrong = [];
  for (const f of frames) {
    const title = `Probe track ${tag}`;
    if (f.barTitle?.startsWith(title) && f.bar === cover) wrong.push({ t: round(f.t, 0), where: "bar" });
    if (f.barTitle?.startsWith(title) && f.lyricsHead === cover) wrong.push({ t: round(f.t, 0), where: "lyricsHead" });
    if (f.barTitle?.startsWith(title) && f.ambient.split(",").includes(cover)) wrong.push({ t: round(f.t, 0), where: "ambient" });
    if (f.npTitle?.startsWith(title) && f.np === cover && f.npOpacity > 0.02) wrong.push({ t: round(f.t, 0), where: "nowPlaying", opacity: f.npOpacity });
  }
  return wrong;
}

function sequenceOf(frames, keys) {
  return frames.reduce((s, f) => { const k = keys.map((n) => f[n]).join("|"); return s.at(-1)?.k === k ? s : [...s, { k, t: round(f.t, 0), ...Object.fromEntries(keys.map((n) => [n, f[n]])) }]; }, []).map(({ k, ...f }) => f);
}

/** Reloads the window and puts the page helpers and the fake-core hook back (a new page, a new fake). */
async function reloadApp(win) {
  win.webContents.reload();
  await new Promise((r) => win.webContents.once("did-finish-load", r));
  await js(win, PAGE_HELPERS);
  await until(win, '__glassProbe.has("view-home")', 15000);
  await sleep(800);
  await catchFake(win);
}

export async function sectionCovers(win, dir) {
  const out = { shaped: ["snapshot.mockMode false (forced re-snapshot) so covers are real: A has asset probeA; B, C, N have only a remote URL until the probe publishes their asset; P has no cover; app://covers served by CDP Fetch"] };
  out.served = await serveCovers(win, { probeA: [220, 40, 40], probeB: [40, 200, 60], probeC: [40, 80, 230], probeN: [230, 200, 40] });
  // A page keeps the loader it got at load time: reload so the override serves its requests.
  await reloadApp(win);
  // The override answers: from the main process and from an image in the page.
  out.serveCheck = {
    main: await win.webContents.session.fetch("app://covers/probeA").then((r) => r.status).catch((e) => String(e)),
    page: await js(win, `new Promise((r) => { const i = new Image(); i.onload = () => r("load " + i.naturalWidth); i.onerror = () => r("error"); i.src = "app://covers/probeA?check"; })`),
  };
  await realArtwork(win);
  for (const view of ["now-playing", "lyrics"]) {
    const r = (out[view] = {});
    await navigate(win, view);
    // Rapid: A, then B at 0 ms and C at 600 ms, neither cached; C's cover arrives at 2600 ms.
    await setTrack(win, "A"); await sleep(1500);
    await startRec(win, COVER_SAMPLER);
    await sleep(100);
    await setTrack(win, "B"); await sleep(600);
    await setTrack(win, "C"); await sleep(2000);
    await coverArrives(win, "C", "probeC"); await sleep(1200);
    let rec = await stopRec(win);
    r.rapid = { aUnderC: coverUnderTitle(rec.frames, "probeA", "C"), aUnderB: coverUnderTitle(rec.frames, "probeA", "B"), sequence: sequenceOf(rec.frames, ["barTitle", "bar", "lyricsHead", "np", "npOpacity", "npTitleOpacity", "ambient"]).slice(0, 40) };
    // Single: A, then N never cached: how long A stays under N, and what Now Playing reveals.
    await setTrack(win, "A"); await sleep(1500);
    await startRec(win, COVER_SAMPLER);
    await sleep(100);
    const t0 = await js(win, "performance.now() - globalThis.__rec.t0");
    await setTrack(win, "N"); await sleep(3200);
    rec = await stopRec(win);
    const aUnderN = coverUnderTitle(rec.frames, "probeA", "N");
    const lastA = (where) => Math.max(-1, ...rec.frames.filter((f) => f.t > t0 && (where === "bar" ? f.bar : where === "ambient" ? (f.ambient.split(",").includes("probeA") ? "probeA" : "") : f.lyricsHead) === "probeA").map((f) => f.t - t0));
    const npRevealAt = rec.frames.find((f) => f.t > t0 + 30 && f.npTitle?.startsWith("Probe track N") && f.npTitleOpacity > 0.05);
    r.single = { heldMs: { bar: round(lastA("bar"), 0), lyricsHead: round(lastA("lyricsHead"), 0), ambient: round(lastA("ambient"), 0) }, nowPlayingWrong: aUnderN.filter((w) => w.where === "nowPlaying"), npRevealMs: npRevealAt ? round(npRevealAt.t - t0, 0) : null, npRevealWith: npRevealAt?.np ?? null, sequence: sequenceOf(rec.frames, ["barTitle", "bar", "lyricsHead", "np", "npOpacity", "npTitleOpacity", "ambient"]).slice(0, 30) };
    // No stand-in: P (no cover) then N (uncached): placeholder after about 500 ms.
    await setTrack(win, "P"); await sleep(1500);
    await startRec(win, COVER_SAMPLER);
    await sleep(100);
    const t1 = await js(win, "performance.now() - globalThis.__rec.t0");
    await setTrack(win, "N"); await sleep(1500);
    rec = await stopRec(win);
    const reveal = rec.frames.find((f) => f.t > t1 + 30 && f.npTitle?.startsWith("Probe track N") && f.npTitleOpacity > 0.05);
    r.noStandIn = { npRevealMs: reveal ? round(reveal.t - t1, 0) : null, npRevealWith: reveal?.np ?? null, barAfter: rec.frames.at(-1).bar };
  }
  json(dir, "covers", out);
  return out;
}

const TITLES = {
  one: "Short probe title",
  two: "A probe title long enough to need a second line on this stage",
  long: "An extremely long probe title that keeps going well past two lines on every tier of the window, with more words, more clauses and still more words, so the end must be cut with an ellipsis",
};

export async function sectionTitles(win, dir) {
  const out = { shaped: ["the current track's title is replaced by one-, two- and many-line probe titles"], tiers: {} };
  const tiers = [["compact", 900, 600], ["regular", 1440, 900], ["wide", 1920, 1080], ["ultrawide", 2560, 1080]];
  const titleState = `(() => { const h = document.querySelector('[data-testid="now-playing-title"]'); const s = h.querySelector(".np-title-text"); const lh = parseFloat(getComputedStyle(s).lineHeight); return { tooltip: h.getAttribute("title"), text: s.textContent, clamped: s.scrollHeight > s.clientHeight + 1, lines: Math.round(s.getBoundingClientRect().height / lh), boxHeight: Math.round(h.getBoundingClientRect().height), textOverflow: getComputedStyle(s).textOverflow, clamp: getComputedStyle(s).webkitLineClamp }; })()`;
  await navigate(win, "now-playing");
  for (const [tier, w, h] of tiers) {
    win.setContentSize(w, h);
    await sleep(600);
    const r = (out.tiers[tier] = { size: win.getContentSize() });
    let n = 0;
    const show = (key) => publishPlayer(win, `(p, f) => { p.currentTrack = { ...f.tracks10k[300 + ${n++}], title: ${JSON.stringify(TITLES[key])} }; p.trackInstanceId = "probe-title-" + Date.now(); }`);
    await show("one"); await sleep(1200);
    r.states = { one: await js(win, titleState) };
    const head = await box(win, ".np-info");
    if (head) await win.webContents.capturePage({ x: Math.max(0, Math.floor(head.x) - 8), y: Math.max(0, Math.floor(head.y) - 140), width: Math.min(Math.ceil(head.w) + 16, w), height: Math.min(Math.ceil(head.h) + 148, 500) }).then((img) => writeFileSync(join(dir, `np-title-one-${tier}.png`), img.toPNG()));
    await startRec(win, NP_GEOMETRY);
    for (const key of ["two", "long", "one"]) {
      await sleep(50);
      await show(key);
      await sleep(1100);
      if (key !== "one") r.states[key] = await js(win, titleState);
      if (key === "long" && head) await win.webContents.capturePage({ x: Math.max(0, Math.floor(head.x) - 8), y: Math.max(0, Math.floor(head.y) - 140), width: Math.min(Math.ceil(head.w) + 16, w), height: Math.min(Math.ceil(head.h) + 148, 500) }).then((img) => writeFileSync(join(dir, `np-title-long-${tier}.png`), img.toPNG()));
    }
    const rec = await stopRec(win);
    r.barLike = geometryDrift(rec.frames, "barLike");
    r.cover = geometryDrift(rec.frames, "cover"); r.lyrics = geometryDrift(rec.frames, "lyrics"); r.upNext = geometryDrift(rec.frames, "upNext"); r.title = geometryDrift(rec.frames, "title");
    r.cls = round(rec.cls, 4);
    r.shifts = rec.shifts.map((x) => ({ t: round(x.t, 0), value: round(x.value, 4), sources: x.sources }));
  }
  win.setContentSize(1280, 800);
  json(dir, "titles", out);
  return out;
}

const MENU_OPEN = '__glassProbe.has("track-actions")';
/** The focused element, as TAG:testid (or its text). */
const FOCUSED = `(() => { const a = document.activeElement; return a ? a.tagName + ":" + (a.dataset.testid ?? a.textContent.trim().slice(0, 24)) : null; })()`;
/** How far the row travelled while the menu was open, from a __tail recording. */
const travelWhileOpen = (tail) => {
  const open = tail.filter((f) => f.open);
  const gaps = open.map((f) => f.menuToButton).filter((g) => g != null);
  return open.length ? { frames: open.length, rowY: [Math.min(...open.map((f) => f.rowY)), Math.max(...open.map((f) => f.rowY))], menuTopMinusButtonBottom: gaps.length ? [Math.min(...gaps), Math.max(...gaps)] : null } : null;
};

export async function sectionMenu(win, dir) {
  const out = {};
  const openOn = async (selector) => {
    await realClick(win, selector);
    await until(win, MENU_OPEN, 3000);
    await sleep(350);
  };
  const closeIfOpen = async () => { if (await js(win, MENU_OPEN)) { await key(win, "Escape"); await sleep(500); } };
  const after = async (ms = 700) => { await sleep(ms); return js(win, MENU_OPEN); };
  await openLongList(win, 2000);
  await rowHeartSelector(win, null);
  const actions = '[data-probe-row] [data-testid="track-actions-btn"]';
  // A scroll event nobody made, with no movement (a list update that clamps nothing).
  await openOn(actions);
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').dispatchEvent(new Event("scroll")); return true; })()`);
  out.scrollEventNoMove = { open: await after() };
  // A programmatic smooth scroll that moves the row (a smooth-scroll tail): closes because the row moved.
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollBy({ top: 120, behavior: "smooth" }); return true; })()`);
  out.smoothTailMovingRow = { open: await after() };
  await closeIfOpen();
  // A smooth-scroll tail that started before the menu opened and ends before reaching the row: the row moves, so it closes;
  // one that does not move the row (the page at its end) cannot be produced on a list page: see shelf.
  // A real smooth-scroll tail: one wheel notch, then the row's actions button pressed 50 ms later.
  await rowHeartSelector(win, null);
  const tailPage = await box(win, '[data-testid="main-scroll-region"]');
  const tx = Math.round(tailPage.x + 600), ty = Math.round(tailPage.y + 300);
  await js(win, `(() => { window.__tail = []; const m = document.querySelector('[data-testid="main-scroll-region"]'); const row = document.querySelector("[data-probe-row]"); const t0 = performance.now(); const f = () => { window.__tail.push({ t: Math.round(performance.now() - t0), top: Math.round(m.scrollTop), rowY: Math.round(row.getBoundingClientRect().top), open: !!document.querySelector('[data-testid="track-actions"]'), menuToButton: (() => { const mm = document.querySelector('[data-testid="track-actions-menu"]'); const b = row.querySelector('[data-testid="track-actions-btn"]'); return mm && b && mm.style.top ? Math.round((parseFloat(mm.style.top) - b.getBoundingClientRect().bottom) * 10) / 10 : null; })() }); if (performance.now() - t0 < 900) requestAnimationFrame(f); }; requestAnimationFrame(f); return true; })()`);
  win.webContents.sendInputEvent({ type: "mouseMove", x: tx, y: ty });
  win.webContents.sendInputEvent({ type: "mouseWheel", x: tx, y: ty, deltaX: 0, deltaY: -100, wheelTicksY: -1, canScroll: true });
  await sleep(50);
  await js(win, `(() => { document.querySelector('[data-probe-row] [data-testid="track-actions-btn"]').click(); return true; })()`);
  await sleep(950);
  let tail = await js(win, "window.__tail");
  out.realWheelTailDomClick = { openAfter: await js(win, MENU_OPEN), everOpen: tail.some((f) => f.open), rowTravel: tail.length ? tail[0].rowY - tail.at(-1).rowY : null, whileOpen: travelWhileOpen(tail), scrollTop: [tail[0]?.top, tail.at(-1)?.top] };
  await closeIfOpen();
  // The same with a real pointer press on the moving button (its position read just before).
  await rowHeartSelector(win, null);
  await js(win, `(() => { window.__tail = []; const m = document.querySelector('[data-testid="main-scroll-region"]'); const row = document.querySelector("[data-probe-row]"); const t0 = performance.now(); const f = () => { window.__tail.push({ t: Math.round(performance.now() - t0), top: Math.round(m.scrollTop), rowY: Math.round(row.getBoundingClientRect().top), open: !!document.querySelector('[data-testid="track-actions"]'), menuToButton: (() => { const mm = document.querySelector('[data-testid="track-actions-menu"]'); const b = row.querySelector('[data-testid="track-actions-btn"]'); return mm && b && mm.style.top ? Math.round((parseFloat(mm.style.top) - b.getBoundingClientRect().bottom) * 10) / 10 : null; })() }); if (performance.now() - t0 < 900) requestAnimationFrame(f); }; requestAnimationFrame(f); return true; })()`);
  win.webContents.sendInputEvent({ type: "mouseMove", x: tx, y: ty });
  win.webContents.sendInputEvent({ type: "mouseWheel", x: tx, y: ty, deltaX: 0, deltaY: -100, wheelTicksY: -1, canScroll: true });
  await sleep(50);
  // What the real press landed on (the button is moving with the glide).
  await js(win, `(() => { window.__press = null; addEventListener("pointerdown", (e) => { window.__press = e.target.closest("[data-testid]")?.dataset.testid ?? e.target.tagName; }, { capture: true, once: true }); return true; })()`);
  await realClick(win, '[data-probe-row] [data-testid="track-actions-btn"]');
  await sleep(950);
  tail = await js(win, "window.__tail");
  out.realWheelTail = { openAfter: await js(win, MENU_OPEN), pressedOn: await js(win, "window.__press"), whileOpen: travelWhileOpen(tail), scrollTop: [tail[0]?.top, tail.at(-1)?.top], frames: tail.filter((f, i) => i === 0 || f.top !== tail[i - 1].top || f.open !== tail[i - 1].open).slice(0, 30) };
  // Once that glide has ended, a move nobody made closes the menu again.
  if (out.realWheelTail.openAfter) {
    await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop += 200; return true; })()`);
    out.realWheelTail.moveAfterGlideOpen = await after();
  }
  await closeIfOpen();
  // Wheel outside the menu.
  await rowHeartSelector(win, null);
  await openOn(actions);
  const page = await box(win, '[data-testid="main-scroll-region"]');
  // Over the page's own content, left of the list's action column and away from the sidebar.
  const content = await box(win, '[data-testid="virtual-track-list"]');
  const wx = Math.round(Math.max(content.x, 0) + 200), wy = Math.round(page.y + 120);
  await js(win, `(() => { window.__menuWheel = []; addEventListener("wheel", (e) => window.__menuWheel.push({ target: e.target.closest("[data-testid]")?.dataset.testid ?? e.target.tagName, prevented: e.defaultPrevented }), { capture: false, passive: true, once: true }); return true; })()`);
  const wheelTop = await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`);
  const atPoint = await js(win, `(() => { const el = document.elementFromPoint(${wx}, ${wy}); return el?.closest("[data-testid]")?.dataset.testid ?? el?.tagName ?? null; })()`);
  win.webContents.sendInputEvent({ type: "mouseMove", x: wx, y: wy });
  win.webContents.sendInputEvent({ type: "mouseWheel", x: wx, y: wy, deltaX: 0, deltaY: -100, wheelTicksY: -1, canScroll: true });
  out.wheel = { open: await after(), elementAtPoint: atPoint, events: await js(win, "window.__menuWheel"), scrollTop: [wheelTop, await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`)] };
  if (out.wheel.open) await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "menu-after-wheel.png"), img.resize({ width: 1280 }).toPNG()));
  await closeIfOpen();
  // A scroll key (focus is inside the menu when it opens).
  await rowHeartSelector(win, null);
  await openOn(actions);
  out.scrollKey = { focusIn: await js(win, `document.activeElement.closest('[data-testid="track-actions-menu"]') ? "menu" : (document.activeElement.dataset.testid ?? document.activeElement.tagName)`) };
  const top0 = await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`);
  await key(win, "PageDown");
  out.scrollKey.open = await after();
  out.scrollKey.pageScrolled = (await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`)) !== top0;
  await closeIfOpen();
  // A scroll key after opening the menu with a right-click on the row.
  await rowHeartSelector(win, null);
  const rowBox = await box(win, "[data-probe-row]");
  const rx = Math.round(rowBox.x + rowBox.w * 0.45), ry = Math.round(rowBox.y + rowBox.h / 2);
  win.webContents.sendInputEvent({ type: "mouseMove", x: rx, y: ry });
  win.webContents.sendInputEvent({ type: "mouseDown", x: rx, y: ry, button: "right", clickCount: 1 });
  win.webContents.sendInputEvent({ type: "mouseUp", x: rx, y: ry, button: "right", clickCount: 1 });
  win.webContents.sendInputEvent({ type: "contextMenu", x: rx, y: ry });
  await until(win, MENU_OPEN, 3000).catch(() => {});
  await sleep(350);
  out.scrollKeyAfterRightClick = { opened: await js(win, MENU_OPEN), focusIn: await js(win, `document.activeElement.closest('[data-testid="track-actions-menu"]') ? "menu" : (document.activeElement.dataset.testid ?? document.activeElement.tagName)`) };
  const top1 = await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`);
  await key(win, "PageDown");
  out.scrollKeyAfterRightClick.open = await after();
  out.scrollKeyAfterRightClick.pageScrolled = (await js(win, `document.querySelector('[data-testid="main-scroll-region"]').scrollTop`)) !== top1;
  await closeIfOpen();
  // A press on the page's scrollbar.
  await rowHeartSelector(win, null);
  await openOn(actions);
  const m = await js(win, `(() => { const el = document.querySelector('[data-testid="main-scroll-region"]'); const r = el.getBoundingClientRect(); return { x: r.right - 4, y: r.top + r.height / 2, bar: el.offsetWidth - el.clientWidth }; })()`);
  win.webContents.sendInputEvent({ type: "mouseMove", x: Math.round(m.x), y: Math.round(m.y) });
  win.webContents.sendInputEvent({ type: "mouseDown", x: Math.round(m.x), y: Math.round(m.y), button: "left", clickCount: 1 });
  win.webContents.sendInputEvent({ type: "mouseUp", x: Math.round(m.x), y: Math.round(m.y), button: "left", clickCount: 1 });
  out.scrollbarPress = { scrollbarWidth: m.bar, open: await after() };
  await closeIfOpen();
  // The row moves (an instant scroll by script).
  await rowHeartSelector(win, null);
  await openOn(actions);
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop += 200; return true; })()`);
  out.rowMoved = { open: await after() };
  await closeIfOpen();
  // The shelf settling on Home while a menu from a Recently played row is open.
  await navigate(win, "home");
  await js(win, "__glassProbe.lengthenShelf(6)");
  await js(win, `(() => { document.querySelectorAll("[data-probe-row]").forEach((e) => e.removeAttribute("data-probe-row")); const row = document.querySelector('[data-testid="view-home"] [data-testid="virtual-track-row"]'); row.setAttribute("data-probe-row", ""); return true; })()`);
  await openOn(actions);
  await js(win, `(() => { document.querySelector('[data-testid="cover-shelf"]').scrollTo({ left: 400, behavior: "smooth" }); return true; })()`);
  out.shelfSettling = { open: await after(900), shelfLeft: await js(win, `document.querySelector('[data-testid="cover-shelf"]').scrollLeft`) };
  await closeIfOpen();
  // The row removed: a queue row whose entry leaves the queue.
  await call(win, shapeQueue);
  await navigate(win, "queue");
  await sleep(400);
  await js(win, `(() => { document.querySelectorAll("[data-probe-row]").forEach((e) => e.removeAttribute("data-probe-row")); const rows = document.querySelectorAll('[data-testid="queue-up-next"] [data-testid="virtual-track-row"]'); rows[3].setAttribute("data-probe-row", ""); return rows[3].dataset.rowKey; })()`);
  await openOn(actions);
  const removedKey = await js(win, `document.querySelector("[data-probe-row]").dataset.rowKey`);
  await js(win, `(() => { const f = globalThis.__probe.fake; const s = f.snapshot; const q = { ...s.queueSummary, queueRevision: s.queueSummary.queueRevision + 1, previewEntries: s.queueSummary.previewEntries.filter((e) => e.entryId !== ${JSON.stringify(removedKey)}) }; q.loadedCount = q.previewEntries.length; s.queueSummary = q; f.emit({ deltaType: "queueReplaced", queue_summary: structuredClone(q), revision: ++s.revision }); return true; })()`);
  out.rowRemoved = { key: removedKey, open: await after(), focus: await js(win, FOCUSED), openerConnected: await js(win, `!!document.querySelector('[data-testid="virtual-track-row"][data-row-key=${JSON.stringify(removedKey)}]')`), menuTitle: await js(win, `document.querySelector('[data-testid="track-actions"]')?.textContent.trim().slice(0, 60) ?? null`) };
  if (out.rowRemoved.open) await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "menu-after-row-removed.png"), img.resize({ width: 1280 }).toPNG()));
  await closeIfOpen();
  // An entry before the menu's row removed: the row moves up without any scroll.
  await js(win, `(() => { document.querySelectorAll("[data-probe-row]").forEach((e) => e.removeAttribute("data-probe-row")); const rows = document.querySelectorAll('[data-testid="queue-up-next"] [data-testid="virtual-track-row"]'); rows[4].setAttribute("data-probe-row", ""); return true; })()`);
  await openOn(actions);
  const yBefore = await js(win, `document.querySelector("[data-probe-row]").getBoundingClientRect().top`);
  await js(win, `(() => { const f = globalThis.__probe.fake; const s = f.snapshot; const e = s.queueSummary.previewEntries; const cur = e.findIndex((x) => x.entryId === s.queueSummary.currentEntryId); const q = { ...s.queueSummary, queueRevision: s.queueSummary.queueRevision + 1, previewEntries: e.filter((_, i) => i !== cur + 1) }; q.loadedCount = q.previewEntries.length; s.queueSummary = q; f.emit({ deltaType: "queueReplaced", queue_summary: structuredClone(q), revision: ++s.revision }); return true; })()`);
  await sleep(400);
  out.rowMovedNoScroll = { open: await js(win, MENU_OPEN), rowY: [round(yBefore, 1), await js(win, `(() => { const r = document.querySelector("[data-probe-row]"); return r?.isConnected ? Math.round(r.getBoundingClientRect().top * 10) / 10 : null; })()`)], rowConnected: await js(win, `!!document.querySelector("[data-probe-row]")`) };
  await sleep(300);
  out.rowMovedNoScroll.openLater = await js(win, MENU_OPEN);
  out.rowMovedNoScroll.focus = await js(win, FOCUSED);
  await closeIfOpen();
  // A queue update that does not move the menu's row (an entry after it removed).
  await js(win, `(() => { document.querySelectorAll("[data-probe-row]").forEach((e) => e.removeAttribute("data-probe-row")); const rows = document.querySelectorAll('[data-testid="queue-up-next"] [data-testid="virtual-track-row"]'); rows[1].setAttribute("data-probe-row", ""); return true; })()`);
  await openOn(actions);
  await js(win, `(() => { const f = globalThis.__probe.fake; const s = f.snapshot; const e = s.queueSummary.previewEntries; const q = { ...s.queueSummary, queueRevision: s.queueSummary.queueRevision + 1, previewEntries: e.slice(0, -1) }; q.loadedCount = q.previewEntries.length; s.queueSummary = q; f.emit({ deltaType: "queueReplaced", queue_summary: structuredClone(q), revision: ++s.revision }); return true; })()`);
  out.updateNotMovingRow = { open: await after() };
  await closeIfOpen();
  json(dir, "menu", out);
  return out;
}

export async function sectionPreparing(win, dir) {
  const out = { shaped: ["player.currentTrack null with player.preparing true, then false"] };
  const state = `(() => { const b = document.querySelector('[data-testid="btn-play-pause"]'); return { disabled: b.disabled, title: b.title, label: b.getAttribute("aria-label") }; })()`;
  await navigate(win, "home");
  for (const lang of ["en", "zh-CN"]) {
    await js(win, `(() => { globalThis.__probe.fake.updateSettings({ uiLanguage: "${lang}" }); return true; })()`);
    await sleep(500);
    await publishPlayer(win, `(p) => { p.currentTrack = null; p.currentQueueEntryId = null; p.playback = "idle"; p.preparing = true; }`);
    await sleep(400);
    const preparing = await js(win, state);
    if (lang === "en") { const b = await box(win, '[data-testid="btn-play-pause"]'); await win.webContents.capturePage({ x: Math.round(b.x) - 300, y: Math.round(b.y) - 20, width: 600, height: Math.round(b.h) + 40 }).then((img) => writeFileSync(join(dir, "play-preparing-en.png"), img.toPNG())); }
    await publishPlayer(win, `(p) => { p.preparing = false; }`);
    await sleep(400);
    out[lang] = { preparing, noTrack: await js(win, state) };
  }
  // Pressing play while preparing sends play.
  await js(win, `(() => { globalThis.__probe.fake.updateSettings({ uiLanguage: "en" }); return true; })()`);
  await publishPlayer(win, `(p) => { p.preparing = true; }`);
  await sleep(300);
  const from = await js(win, "globalThis.__probe.log.length");
  await realClick(win, '[data-testid="btn-play-pause"]');
  await sleep(500);
  out.pressWhilePreparing = await js(win, `globalThis.__probe.log.slice(${from}).filter((l) => l.name === "sendPlayerCommand").map((l) => l.arg.op)`);
  await publishPlayer(win, `(p, f) => { p.preparing = false; p.currentTrack = f.tracks10k[0]; p.currentQueueEntryId = "qe-mock-1"; p.playback = "paused"; }`);
  json(dir, "preparing", out);
  return out;
}

const submitSearch = (text) => `(() => { const i = document.querySelector('[data-testid="search-input"]'); i.value = ${JSON.stringify(text)}; i.dispatchEvent(new Event("input", { bubbles: true })); i.closest("form").requestSubmit(); return true; })()`;
const chips = `[...document.querySelectorAll('[data-testid="search-recent-chip"]')].map((c) => c.textContent.trim())`;

export async function sectionRecent(win, dir) {
  const out = {};
  await navigate(win, "search");
  await sleep(300);
  out.initial = await js(win, chips);
  for (const q of ["Neon", "夜の硝子"]) { await js(win, submitSearch(q)); await sleep(800); }
  await js(win, clickTestid("search-clear-btn"));
  await sleep(600);
  out.afterTwo = await js(win, chips);
  out.fakeList = await js(win, `[...globalThis.__probe.fake.recentSearchesByScope.values()].flat().map((i) => i.query)`);
  await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "recent-chips.png"), img.resize({ width: 1280 }).toPNG()));
  await js(win, clickTestid("search-recent-clear"));
  await sleep(600);
  out.afterClear = await js(win, chips);
  // A list arriving while the start blocks show: recorded in the fake directly, the UI asked
  // again by an accountState invalidation, the reply 1200 ms late.
  await js(win, `(() => { const f = globalThis.__probe.fake; const e = f.snapshot.authState.accountEpoch; f.recentSearches({ action: "record", query: "Late one" }, e); f.recentSearches({ action: "record", query: "Late two" }, e); return true; })()`);
  await setLatency(win, { recent: 1200 });
  await startRec(win, `() => { const b = document.querySelector('[data-testid="search-start"]'); const blocks = [...b.children].map((c) => Math.round(c.getBoundingClientRect().top)); return { chips: document.querySelectorAll('[data-testid="search-recent-chip"]').length, tops: blocks.join(",") }; }`);
  await js(win, `(() => { const f = globalThis.__probe.fake; f.emit({ deltaType: "resourceInvalidated", resource_kind: "accountState", revision: ++f.snapshot.revision }); return true; })()`);
  await sleep(2000);
  let rec = await stopRec(win);
  out.lateArrival = { frames: rec.frames.length, firstFrame: rec.frames[0] ?? null, sequence: sequenceOf(rec.frames, ["chips", "tops"]), cls: round(rec.cls, 4), shifts: rec.shifts, recentRequests: await js(win, `globalThis.__probe.log.filter((l) => l.name === "recentSearches").map((l) => ({ action: l.arg.action, t: Math.round(l.t - globalThis.__rec.t0), ms: l.ms }))`) };
  await navigate(win, "home");
  await navigate(win, "search");
  await sleep(400);
  out.lateArrival.afterReturn = await js(win, chips);
  await setLatency(win, { recent: 0 });
  // Reload the window: the fake core lives in the page, so this is a new fake.
  await reloadApp(win);
  await navigate(win, "search");
  await sleep(500);
  out.afterReload = await js(win, chips);
  json(dir, "recent", out);
  return out;
}

/** Text contrast against the hardest pixel behind it: the text is made transparent and its box captured. */
async function textContrast(win, selector) {
  // The box of the element's own text nodes only (a mark or icon beside the text is not behind it).
  const info = await js(win, `(() => { document.querySelectorAll("[data-probe-text]").forEach((e) => e.removeAttribute("data-probe-text")); const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return null; el.setAttribute("data-probe-text", ""); const rects = []; for (const n of el.childNodes) if (n.nodeType === 3 && n.textContent.trim()) { const range = document.createRange(); range.selectNodeContents(n); rects.push(...range.getClientRects()); } if (!rects.length) return null; const x0 = Math.min(...rects.map((r) => r.left)), y0 = Math.min(...rects.map((r) => r.top)), x1 = Math.max(...rects.map((r) => r.right)), y1 = Math.max(...rects.map((r) => r.bottom)); const c = getComputedStyle(el); return { rect: [x0, y0, x1 - x0, y1 - y0], color: c.color, size: parseFloat(c.fontSize), weight: Number(c.fontWeight), text: el.textContent.trim().slice(0, 30) }; })()`);
  if (!info) return { error: `no ${selector}` };
  const canvasRgb = await js(win, `(() => { const c = document.createElement("canvas").getContext("2d"); c.fillStyle = ${JSON.stringify(info.color)}; c.fillRect(0, 0, 1, 1); return [...c.getImageData(0, 0, 1, 1).data]; })()`);
  await js(win, `(() => { const el = document.querySelector("[data-probe-text]"); el.dataset.probeColor = el.style.color; el.style.setProperty("color", "transparent", "important"); return true; })()`);
  await sleep(80);
  const [x, y, w, h] = info.rect;
  const shot = await grab(win, { x: Math.floor(x), y: Math.floor(y), width: Math.ceil(w), height: Math.ceil(h) });
  await js(win, `(() => { const el = document.querySelector("[data-probe-text]"); el.style.color = el.dataset.probeColor; el.removeAttribute("data-probe-text"); return true; })()`);
  // Alpha in the computed colour is composited over each pixel.
  const a = canvasRgb[3] / 255;
  let worst = Infinity;
  for (let py = 0; py < shot.ph; py++) for (let px = 0; px < shot.pw; px++) {
    const bg = shot.px(px, py);
    const ink = [0, 1, 2].map((i) => canvasRgb[i] * a + bg[i] * (1 - a));
    worst = Math.min(worst, ratio(lum(ink), lum(bg)));
  }
  const large = info.size >= 24 || (info.size >= 18.66 && info.weight >= 700);
  return { text: info.text, color: info.color, min: large ? 3 : 4.5, ratio: round(worst) };
}

/** Contrast of a small mark (an element, not an svg) against what is behind it, as iconContrast does. */
async function markContrast(win, selector) {
  const r = await box(win, selector);
  if (!r) return { error: `no ${selector}` };
  const rect = { x: Math.floor(r.x) - 1, y: Math.floor(r.y) - 1, width: Math.ceil(r.w) + 2, height: Math.ceil(r.h) + 2 };
  const withMark = await grab(win, rect);
  await js(win, `(() => { document.querySelector(${JSON.stringify(selector)}).style.visibility = "hidden"; return true; })()`);
  await sleep(80);
  const without = await grab(win, rect);
  await js(win, `(() => { document.querySelector(${JSON.stringify(selector)}).style.visibility = ""; return true; })()`);
  const samples = [];
  let maxDiff = 0;
  for (let y = 0; y < withMark.ph; y++) for (let x = 0; x < withMark.pw; x++) { const a = withMark.px(x, y), b = without.px(x, y); const d = Math.abs(lum(a) - lum(b)); samples.push({ d, a, b }); maxDiff = Math.max(maxDiff, d); }
  const solid = samples.filter((s) => s.d >= maxDiff * 0.9).map((s) => ratio(lum(s.a), lum(s.b))).sort((p, q) => p - q);
  return { min: 3, solidMedian: round(solid[Math.floor(solid.length / 2)]), solidMin: round(solid[0]), pixels: solid.length };
}

export async function sectionProblems(win, dir) {
  const out = { shaped: ["next fails as core reports a failed receipt (TIMEOUT); a like's write answers indeterminate"] };
  const settingsGeometry = `(() => { const b = document.querySelector('[data-testid="settings-open-diagnostics"]'); const row = b.parentElement; return { button: (({ x, y, width, height }) => [x, y, width, height].map(Math.round))(b.getBoundingClientRect()), siblings: [...row.children].filter((c) => c !== b).map((c) => (({ x, y, width, height }) => [c.dataset.testid ?? c.textContent.trim().slice(0, 16), ...[x, y, width, height].map(Math.round)])(c.getBoundingClientRect())) }; })()`;
  await showDiagnosticsEntryLocal(win);
  out.settingsBefore = await js(win, settingsGeometry);
  // A command failure.
  await setProbe(win, `{ failOps: ["next"] }`);
  await navigate(win, "home");
  await realClick(win, '[data-testid="btn-next"]');
  await sleep(600);
  out.commandToast = await js(win, `[...document.querySelectorAll(".toast")].map((t) => t.textContent.trim().slice(0, 100))`);
  await setProbe(win, `{ failOps: [] }`);
  await until(win, '!document.querySelector(".toast")', 15000).catch(() => {});
  // An unknown write.
  await js(win, `(() => { globalThis.__probe.shape.mutation = (f, req) => req.kind === "setLike" ? { kind: req.kind, outcome: "indeterminate", error: { code: "TIMEOUT", safeMessage: "Probe: the write was not confirmed", retriable: false } } : null; return true; })()`);
  await navigate(win, "library");
  await js(win, clickTestid("playlist-card-mock-pl-favorites"));
  await until(win, `document.querySelector('[data-testid="virtual-track-row"]')`).catch(async (err) => {
    await win.webContents.capturePage().then((img) => writeFileSync(join(dir, "problems-stuck.png"), img.resize({ width: 1280 }).toPNG()));
    json(dir, "problems-stuck", { log: await js(win, "globalThis.__probe.log.slice(-12).map((l) => ({ name: l.name, kind: l.arg?.kind, op: l.arg?.op, ms: l.ms }))"), views: await js(win, `[...document.querySelectorAll('[data-testid^="view-"], [data-testid$="-state-loading"], [data-testid$="-state-error"]')].map((e) => e.dataset.testid)`), toasts: await js(win, `[...document.querySelectorAll(".toast")].map((t) => t.textContent.trim())`), partial: out });
    throw err;
  });
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 400; return true; })()`);
  await sleep(400);
  await rowHeartSelector(win, null);
  await realClick(win, HEART);
  await sleep(700);
  out.writeToast = await js(win, `[...document.querySelectorAll(".toast")].map((t) => t.textContent.trim().slice(0, 100))`);
  await js(win, "(globalThis.__probe.shape.mutation = null, true)");
  await until(win, '!document.querySelector(".toast")', 20000).catch(() => {});
  for (const theme of ["dark", "light"]) {
    await setAppearance(win, { theme });
    await showDiagnosticsEntryLocal(win);
    await sleep(300);
    const t = (out[theme] = { settings: await js(win, settingsGeometry), count: await js(win, `(() => { const c = document.querySelector('[data-testid="settings-problem-count"]'); if (!c) return null; const r = c.getBoundingClientRect(); const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2); let clipped = false; for (let n = c.parentElement; n; n = n.parentElement) { const cs = getComputedStyle(n); if (cs.overflow !== "visible") { const p = n.getBoundingClientRect(); if (r.left < p.left || r.right > p.right || r.top < p.top || r.bottom > p.bottom) clipped = true; } } return { text: c.textContent.trim(), tone: c.dataset.tone, rect: [r.x, r.y, r.width, r.height].map(Math.round), hitIsCount: c.contains(hit), clippedByAncestor: clipped, label: document.querySelector('[data-testid="settings-open-diagnostics"]').getAttribute("aria-label") }; })()`) });
    const b = await box(win, '[data-testid="settings-open-diagnostics"]');
    if (b) await win.webContents.capturePage({ x: Math.round(b.x) - 40, y: Math.round(b.y) - 24, width: Math.round(b.w) + 280, height: Math.round(b.h) + 48 }).then((img) => writeFileSync(join(dir, `settings-count-${theme}.png`), img.toPNG()));
    t.countContrast = { text: await textContrast(win, '[data-testid="settings-problem-count"]'), mark: await markContrast(win, ".settings-problem-mark") };
    await js(win, '__glassProbe.click("settings-open-diagnostics")');
    await until(win, '__glassProbe.has("diagnostics-problems")');
    await sleep(700);
    t.entries = await js(win, `[...document.querySelectorAll('[data-testid="problem-entry"]')].map((e) => ({ text: e.textContent.trim().replace(/\\s+/g, " ").slice(0, 120), unchecked: e.hasAttribute("data-unchecked"), notChecked: !!e.querySelector('[data-testid="problem-not-checked"]') }))`);
    const sec = await box(win, '[data-testid="diagnostics-problems"]');
    if (sec) await win.webContents.capturePage({ x: Math.max(0, Math.round(sec.x) - 12), y: Math.max(0, Math.round(sec.y) - 12), width: Math.round(sec.w) + 24, height: Math.min(Math.round(sec.h) + 24, 600) }).then((img) => writeFileSync(join(dir, `diagnostics-problems-${theme}.png`), img.toPNG()));
    t.contrast = {};
    for (const [name, sel] of [["heading", "#recent-problems-heading"], ["entry text", ".recent-problem-head p"], ["entry time", ".recent-problem-head time"], ["advice", '[data-testid="problem-entry"] p[style*="text-secondary"]'], ["not checked", '[data-testid="problem-not-checked"]'], ["details summary", '[data-testid="problem-entry"] summary']]) t.contrast[name] = await textContrast(win, sel);
    t.contrast["entry mark"] = await markContrast(win, ".recent-problem-mark");
    // Keyboard: Tab until a details summary has focus, open it with Enter.
    let reached = null;
    t.focusSequence = [];
    for (let i = 0; i < 40 && !reached; i++) {
      await key(win, "Tab");
      const f = await js(win, `(() => { const a = document.activeElement; return { tag: a.tagName, id: a.dataset.testid ?? a.textContent.trim().slice(0, 18), inDialog: !!a.closest('[data-testid="accessible-dialog"]') }; })()`);
      t.focusSequence.push(`${f.tag}:${f.id}${f.inDialog ? "" : "(outside)"}`);
      reached = f.tag === "SUMMARY" ? true : null;
      if (reached) t.tabsToDetails = i + 1;
    }
    if (reached) {
      // Shift+Tab leaves Details backwards, Tab comes back to it. Names carry the element's
      // place in the dialog (#n), as every entry has its own Details.
      const named = `(() => { const a = document.activeElement; const d = a.closest('[data-testid="accessible-dialog"]'); return a.tagName + ":" + (a.dataset.testid ?? a.textContent.trim().slice(0, 18)) + (d ? "#" + [...d.querySelectorAll("*")].indexOf(a) : "(outside)"); })()`;
      const startAt = await js(win, named);
      await key(win, "Tab", ["shift"]);
      t.shiftTabFromDetails = await js(win, named);
      await key(win, "Tab");
      t.tabBackToDetails = await js(win, named);
      // The trap still wraps: Tab from Details round the whole dialog back to it, and Shift+Tab from the first control.
      t.wrapForward = [startAt];
      for (let i = 0; i < 20; i++) {
        await key(win, "Tab");
        const n = await js(win, named);
        t.wrapForward.push(n);
        if (n === startAt) break;
      }
      await js(win, `(() => { document.querySelector('[data-testid="dialog-close-btn"]').focus(); return true; })()`);
      await key(win, "Tab", ["shift"]);
      t.shiftTabFromClose = await js(win, named);
      await js(win, `(() => { document.querySelector('[data-testid="problem-entry"] summary').focus(); return true; })()`);
    }
    // Enter activates on its character event, which sendInputEvent sends only when asked.
    if (reached) { win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Enter" }); win.webContents.sendInputEvent({ type: "char", keyCode: "\r" }); win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Enter" }); await sleep(350); t.technicalVisible = await js(win, `(() => { const p = document.activeElement.closest("details")?.querySelector('[data-testid="problem-technical"]'); return p ? { open: p.closest("details").open, visible: p.checkVisibility(), text: p.textContent.trim().slice(0, 80), focusVisible: document.activeElement.matches(":focus-visible") } : null; })()`); }
    if (!reached) {
      // Not reachable by Tab: open it by pointer to measure the detail's contrast.
      await realClick(win, '[data-testid="problem-entry"] summary');
      await sleep(250);
      t.technicalOpenedByClick = await js(win, `!!document.querySelector('[data-testid="problem-entry"] details[open]')`);
      t.summaryFocusableByScript = await js(win, `(() => { const s = document.querySelector('[data-testid="problem-entry"] summary'); s.focus(); return document.activeElement === s; })()`);
    }
    // With Details focused by keyboard its focus ring sits on the detail's first line; measure the
    // text itself with the ring gone, and say how far the ring reaches.
    t.focusRing = await js(win, `(() => { const a = document.activeElement; const p = document.querySelector('[data-testid="problem-technical"]'); if (a?.tagName !== "SUMMARY" || !p) return null; const cs = getComputedStyle(a); const ring = a.getBoundingClientRect().bottom + parseFloat(cs.outlineOffset) + parseFloat(cs.outlineWidth); return { focusVisible: a.matches(":focus-visible"), ringBottom: Math.round(ring * 10) / 10, technicalTop: Math.round(p.getBoundingClientRect().top * 10) / 10 }; })()`);
    if (t.focusRing?.focusVisible) t.contrast["technical (open, ring showing)"] = await textContrast(win, '[data-testid="problem-technical"]');
    await js(win, `(() => { document.activeElement?.blur(); return true; })()`);
    await sleep(120);
    t.contrast["technical (open)"] = await textContrast(win, '[data-testid="problem-technical"]');
    if (theme === "light") {
      await js(win, `(() => { document.querySelector('[data-testid="problem-mark-checked"]')?.click(); return true; })()`);
      await sleep(300);
      t.afterMarkChecked = await js(win, `[...document.querySelectorAll('[data-testid="problem-entry"]')].map((e) => ({ checked: !!e.querySelector('[data-testid="problem-checked"]'), notChecked: !!e.querySelector('[data-testid="problem-not-checked"]') }))`);
    }
    await js(win, `(() => { document.querySelector('[data-testid="dialog-close-btn"]')?.click(); return true; })()`);
    await sleep(500);
  }
  await setAppearance(win, { theme: "dark" });
  // Last: the countdown while the window is minimised (document.hidden). Under Wayland the
  // window cannot un-minimise itself, so restore, show and focus are tried and their effect recorded.
  await setProbe(win, `{ failOps: ["next"] }`);
  await navigate(win, "home");
  await realClick(win, '[data-testid="btn-next"]');
  await setProbe(win, `{ failOps: [] }`);
  await sleep(600);
  const ticks = `new Promise((r) => { let n = 0; const f = () => { n++; }; const id = setInterval(() => {}, 1000); const loop = () => { f(); if (performance.now() - t0 < 500) requestAnimationFrame(loop); }; const t0 = performance.now(); requestAnimationFrame(loop); setTimeout(() => { clearInterval(id); r(n); }, 600); })`;
  out.hidden = { toastBefore: await js(win, `document.querySelectorAll(".toast").length`) };
  win.minimize();
  await sleep(400);
  out.hidden.whileMinimised = { hidden: await js(win, "document.hidden"), visibility: await js(win, "document.visibilityState"), rafIn600ms: await js(win, ticks) };
  await sleep(4500);
  out.hidden.toastStillThereAfter4900msHidden = await js(win, `document.querySelectorAll(".toast").length`);
  win.restore();
  await sleep(500);
  out.hidden.afterRestore = { hidden: await js(win, "document.hidden"), minimized: win.isMinimized(), rafIn600ms: await js(win, ticks) };
  win.show(); win.focus();
  await sleep(500);
  out.hidden.afterShowFocus = { hidden: await js(win, "document.hidden"), minimized: win.isMinimized(), rafIn600ms: await js(win, ticks) };
  const gone = Date.now();
  await until(win, '!document.querySelector(".toast")', 15000).catch(() => {});
  out.hidden.toastGoneMsAfterShow = Date.now() - gone;
  json(dir, "problems", out);
  return out;
}

async function showDiagnosticsEntryLocal(win) {
  await navigate(win, "settings");
  await until(win, '__glassProbe.has("settings-open-diagnostics")');
  await js(win, `(() => { document.querySelector('[data-testid="settings-open-diagnostics"]').scrollIntoView({ block: "center" }); return true; })()`);
  await sleep(250);
}

const median = (a) => { const s = [...a].filter((v) => v != null).sort((p, q) => p - q); return s.length ? round(s[Math.floor(s.length / 2)], 1) : null; };

/** Time from the press (key or click) to the first frame whose last page is `viewId`, with `ready` true. */
async function paintTime(win, act, viewId, ready = "true") {
  await startRec(win, `() => { const v = [...document.querySelectorAll('[data-testid^="view-"]')].at(-1); return { view: v?.dataset.testid ?? null, ready: v ? Boolean((() => ${ready})()) : false }; }`);
  await sleep(60);
  await act();
  await sleep(700);
  const rec = await stopRec(win);
  const press = rec.marks.find((m) => m.type === "pointerdown" || m.type === "keydown")?.t;
  const first = rec.frames.find((f) => press != null && f.t >= press && f.view === viewId && f.ready);
  return press != null && first ? first.t - press : null;
}

export async function sectionPaint(win, dir) {
  const out = { runs: 12 };
  const escape = async () => { win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Escape" }); win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Escape" }); };
  const navClick = (view) => () => realClick(win, `[data-testid="nav-${view}"]`);
  const forward = [], backKept = [], backReload = [], forwardList = [];
  await call(win, shapeCatalog, EXTRA_PLAYLISTS);
  for (let i = 0; i < out.runs; i++) {
    await navigate(win, "home");
    await sleep(300);
    forward.push(await paintTime(win, navClick("queue"), "view-queue"));
    await sleep(300);
    backKept.push(await paintTime(win, escape, "view-home"));
    await sleep(300);
    // A collection page that reloads on Back: Library left for a playlist, its list invalidated meanwhile.
    await navigate(win, "library");
    await sleep(300);
    forwardList.push(await paintTime(win, () => realClick(win, '[data-testid="playlist-card-mock-pl-late-night"]'), "view-playlist", `v.querySelector('[data-testid="virtual-track-row"]')`));
    await js(win, `(() => { const f = globalThis.__probe.fake; f.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistMembership", revision: ++f.snapshot.revision }); return true; })()`);
    await sleep(300);
    backReload.push(await paintTime(win, escape, "view-library"));
    await sleep(300);
  }
  out.forwardNavToQueueMs = { median: median(forward), all: forward.map((v) => round(v, 1)) };
  out.backToKeptHomeMs = { median: median(backKept), all: backKept.map((v) => round(v, 1)) };
  out.forwardToCachedPlaylistRowsMs = { median: median(forwardList), all: forwardList.map((v) => round(v, 1)) };
  out.backToReloadingLibraryMs = { median: median(backReload), all: backReload.map((v) => round(v, 1)) };
  json(dir, "paint", out);
  return out;
}

export async function sectionLeave(win, dir) {
  const out = { shaped: ["add-to-playlist shaped as in 'write'; invalidation without a playlist id; 300 ms per page"] };
  await call(win, shapeCatalog, EXTRA_PLAYLISTS);
  await call(win, shapeAddToPlaylist);
  await navigate(win, "library");
  await js(win, clickTestid("playlist-card-mock-pl-favorites"));
  await until(win, `document.querySelector('[data-testid="virtual-track-row"]')`);
  for (let i = 0; i < 6; i++) { await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); m.scrollTop = m.scrollHeight; return true; })()`); await sleep(500); }
  await js(win, `(() => { document.querySelector('[data-testid="main-scroll-region"]').scrollTop = 9000; return true; })()`);
  await sleep(400);
  await setLatency(win, { catalog: { playlistTracks: 300 } });
  const from = await js(win, "globalThis.__probe.log.length");
  await js(win, `(() => { const f = globalThis.__probe.fake; f.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistTracks", revision: ++f.snapshot.revision }); return true; })()`);
  await sleep(750);
  const leftAt = await js(win, "performance.now()");
  await js(win, clickTestid("nav-home"));
  await sleep(4000);
  const reqs = await js(win, `globalThis.__probe.log.slice(${from}).filter((l) => l.name === "queryCatalog" && l.arg.kind === "playlistTracks").map((l) => ({ t: Math.round(l.t), cursor: l.arg.cursor ?? null }))`);
  out.requestsBeforeLeaving = reqs.filter((r) => r.t < leftAt).map((r) => r.cursor);
  out.requestsAfterLeaving = reqs.filter((r) => r.t >= leftAt).map((r) => ({ cursor: r.cursor, msAfterLeaving: Math.round(r.t - leftAt) }));
  // Coming back reads it again.
  const back = await js(win, "globalThis.__probe.log.length");
  await navigate(win, "library");
  await js(win, clickTestid("playlist-card-mock-pl-favorites"));
  await sleep(2500);
  out.requestsOnReturn = await js(win, `globalThis.__probe.log.slice(${back}).filter((l) => l.name === "queryCatalog" && l.arg.kind === "playlistTracks").map((l) => l.arg.cursor ?? null)`);
  await setLatency(win, { catalog: {} });
  json(dir, "leave", out);
  return out;
}

export async function sectionLibBack(win, dir) {
  const many = Array.from({ length: 40 }, (_, i) => ({ playlistId: `probe-many-${i}`, browseId: null, title: `Probe Shelf ${i + 1}`, subtitle: "Probe-shaped", trackCount: 10, cover: null }));
  const out = { shaped: ["library has 40 extra probe playlists; playlistMembership invalidated while away (drops the Library page); 1000 ms reply"] };
  await call(win, shapeCatalog, many);
  win.setContentSize(1280, 700);
  for (const variant of ["restore", "wheelCancels"]) {
    await navigate(win, "library");
    await until(win, '__glassProbe.has("playlist-card-probe-many-39")');
    await sleep(300);
    const saved = await js(win, `(() => { const m = document.querySelector('[data-testid="main-scroll-region"]'); m.scrollTop = 900; return m.scrollTop; })()`);
    await sleep(300);
    await js(win, clickTestid("playlist-card-mock-pl-late-night"));
    await until(win, '__glassProbe.has("view-playlist")');
    await sleep(500);
    await js(win, `(() => { const f = globalThis.__probe.fake; f.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistMembership", revision: ++f.snapshot.revision }); return true; })()`);
    await setLatency(win, { catalog: { libraryPlaylists: 1000 } });
    await sleep(200);
    await startRec(win, `() => ({ view: [...document.querySelectorAll('[data-testid^="view-"]')].at(-1)?.dataset.testid ?? null, top: Math.round(document.querySelector('[data-testid="main-scroll-region"]').scrollTop), tiles: document.querySelectorAll('[data-testid^="playlist-card-"]').length, loading: !!document.querySelector('[data-testid$="-state-loading"]') })`);
    await sleep(50);
    win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Escape" }); win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Escape" });
    if (variant === "wheelCancels") {
      await sleep(400);
      // Over the page content (the scroll region starts under the sidebar).
      const page = await box(win, '[data-testid="main-scroll-region"]');
      const wx = Math.round(page.x + 600), wy = Math.round(page.y + 200);
      win.webContents.sendInputEvent({ type: "mouseMove", x: wx, y: wy });
      win.webContents.sendInputEvent({ type: "mouseWheel", x: wx, y: wy, deltaX: 0, deltaY: -100, wheelTicksY: -1, canScroll: true });
    }
    await sleep(1800);
    const rec = await stopRec(win);
    await setLatency(win, { catalog: {} });
    out[variant] = { saved, sequence: sequenceOf(rec.frames, ["view", "top", "tiles", "loading"]).slice(0, 20), final: rec.frames.at(-1), cls: round(rec.cls, 4) };
  }
  win.setContentSize(1280, 800);
  json(dir, "libback", out);
  return out;
}

const SECTIONS = {
  feedback: sectionFeedback, nav: sectionNav, write: sectionWrite, nowplaying: sectionNowPlaying, search: sectionSearch, lyrics: sectionLyrics, lists: sectionLists, seek: sectionSeek,
  covers: sectionCovers, titles: sectionTitles, menu: sectionMenu, preparing: sectionPreparing, recent: sectionRecent, problems: sectionProblems, paint: sectionPaint, leave: sectionLeave, libback: sectionLibBack,
};

/** Runs the named sections (all when none are named), each in a fresh launch-state window. */
export async function runCheck(outDir, args) {
  mkdirSync(outDir, { recursive: true });
  const names = args.filter((a) => a in SECTIONS);
  const win = await launchApp();
  await catchFake(win);
  const env = { dpr: await js(win, "devicePixelRatio"), size: win.getContentSize() };
  const results = { env };
  for (const name of names.length ? names : Object.keys(SECTIONS)) {
    try {
      results[name] = await SECTIONS[name](win, outDir);
      console.log(`[check] ${name} done`);
    } catch (err) {
      results[name] = { error: String(err?.stack ?? err) };
      json(outDir, `${name}-error`, results[name]);
      console.error(`[check] ${name} failed: ${err?.message}`);
    }
  }
  json(outDir, `run-${names.join("-") || "all"}`, results);
}
