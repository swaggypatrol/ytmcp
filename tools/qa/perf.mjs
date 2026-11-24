#!/usr/bin/env node
// T21 performance measurements on the real app (real profile, null sink):
//  - warm start to interactive x N (p50/p95; the first launch is reported separately)
//  - click-to-sound x N, split into IPC/core, page (network/service) wait and PipeWire stream
//  - 60 s scroll frame timing in the real UI at the measured refresh rate
//  - visible playing, hidden playing and hidden paused process-tree CPU, wakeups and PSS
//
//   YTMCP_ALLOW_LIVE=1 node tools/qa/perf.mjs [--target dev|installed] [--starts 20] [--clicks 20]
//
// --target installed runs the INSTALLED resources (/opt/ytmcp/resources: main bundle,
// daemon, UI, bridge, identity) through tools/qa/qa-entry.mjs with the dev tree's Electron of
// the same version (the packaged binary cannot take a QA entry); app.isPackaged is then false.
import { spawn } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { parseArgs } from "node:util";
import { AppInstance, DAEMON, ELECTRON, PROD_MAIN, REPO, waitAppReady } from "./lib/app.mjs";
import { ensureNullSink, fileSha256, percentile, resourceDelta, resourceSample, round, run, sinkInputsFor, sleep, unloadNullSink, NULL_SINK } from "./lib/sys.mjs";
import { collectEnvironment } from "./gates/g0.mjs";
import { frameSample } from "./gates/g1.mjs";
import { quitViaMpris } from "./gates/g2.mjs";
import { uiSearch } from "./gates/common.mjs";

const { values: opt } = parseArgs({
  options: {
    target: { type: "string", default: "dev" },
    starts: { type: "string", default: "20" },
    clicks: { type: "string", default: "20" },
    "scroll-sec": { type: "string", default: "60" },
    "paused-sec": { type: "string", default: "60" },
    query: { type: "string", default: "Debussy Clair de Lune" },
    "glass-only": { type: "boolean", default: false },
  },
});
if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing live perf run without YTMCP_ALLOW_LIVE=1");
  process.exit(2);
}

const RES = "/opt/ytmcp/resources";
const INSTALLED_ELECTRON = "/opt/ytmcp/ytmcp-player";
const target =
  opt.target === "installed"
    ? {
        daemonBin: `${RES}/bin/ytm-daemon`,
        extraEnv: {
          YTMCP_QA_MAIN: `${RES}/app/dist/main.mjs`,
          YTMCP_UI_DIST_DIR: `${RES}/ui-dist`,
          YTMCP_BRIDGE_DIST_DIR: `${RES}/bridge-dist`,
          YTMCP_IDENTITY_PATH: `${RES}/config/app-identity.json`,
        },
      }
    : { daemonBin: DAEMON, extraEnv: { YTMCP_QA_MAIN: PROD_MAIN } };
const workDir = join(process.env.YTMCP_QA_WORKDIR ?? join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ytmcp-qa"), `perf-${Date.now()}`);
const stats = (v) => ({ n: v.length, p50: round(percentile(v, 50), 0), p95: round(percentile(v, 95), 0), min: v.length ? Math.min(...v) : null, max: v.length ? Math.max(...v) : null });

/** Spawn -> main UI interactive (DOM ready, nav enabled, core snapshot rendered). */
async function startToInteractive(i) {
  const t0 = Date.now();
  const app = await AppInstance.launch({ label: `perf-start-${i}`, workDir, ...target });
  let interactiveAt = null;
  while (Date.now() - t0 < 60_000) {
    const ok = await app
      .evalMain(`document.readyState === "complete" && !!document.querySelector('[data-testid="nav-search"]') && !!document.querySelector('[data-testid="player-bar"]') && !document.querySelector('[data-testid="toast-unconnected"]')`)
      .catch(() => false);
    const st = await app.state().catch(() => null);
    if (ok && st?.snapshotCount > 0) {
      interactiveAt = Date.now();
      break;
    }
    await sleep(25);
  }
  const ready = await waitAppReady(app, { timeoutMs: 60_000 });
  const bridgeReadyMs = ready.ok ? Date.now() - t0 : null;
  const q = await quitViaMpris(app);
  if (q.leftoverPids.length) await app.kill();
  return { interactiveMs: interactiveAt ? interactiveAt - t0 : null, bridgeReadyMs, quitMs: q.quitMs };
}

const SEARCH_LIST = '[data-testid="view-search"] [data-testid="virtual-track-list"]';
const FIRST_PLAYABLE = `${SEARCH_LIST} button[data-row-index]:not([aria-disabled="true"])`;
const PLAY_BUTTON = '[data-testid="btn-play-pause"]';
const VIDEO_ID = 'document.querySelector("#movie_player")?.getVideoData?.()?.video_id ?? null';
const PAGE_PLAYBACK_JS = `(() => {
  if (location.origin !== "https://music.youtube.com") return null;
  const video = document.querySelector("video");
  return { videoId: ${VIDEO_ID}, seconds: video?.currentTime ?? null, paused: video?.paused ?? true };
})()`;
const PAGE_TIMING_JS = `(() => {
  const controller = new AbortController();
  const events = [];
  const loads = new WeakMap();
  let loadSeq = 0;
  window.__qaPerfTiming = { controller, events };
  for (const t of ["loadstart", "waiting", "canplay", "playing", "pause"]) {
    document.addEventListener(t, (event) => {
      const video = event.target;
      if (video !== document.querySelector("video")) return;
      if (t === "loadstart") loads.set(video, ++loadSeq);
      events.push({ t, at: performance.timeOrigin + performance.now(), videoId: ${VIDEO_ID}, load: loads.get(video) ?? null });
    }, { capture: true, signal: controller.signal });
  }
})()`;

async function startPeakMeter() {
  const binary = join(workDir, "peak-meter");
  const build = await run("cc", ["-std=c17", "-Wall", "-Wextra", "-Werror", "-O2", "-o", binary,
    join(REPO, "tools/qa/lib/peak-meter.c"), "-lpulse"]);
  if (!build.ok) throw new Error(`null sink peak meter build failed: ${build.stderr.trim()}`);
  const child = spawn(binary, [], { stdio: ["ignore", "pipe", "inherit"] });
  let error;
  child.on("error", (cause) => { error = cause; });
  const closed = new Promise((resolve) => child.once("close", resolve));
  const lines = createInterface({ input: child.stdout });
  lines.on("error", (cause) => { error = cause; });
  let state = null;
  let since = null;
  let updatedAt = null;
  lines.on("line", (line) => {
    if (line !== "active" && line !== "quiet") return;
    updatedAt = Date.now();
    if (line !== state) since = updatedAt;
    state = line;
  });
  return {
    read() {
      if (error || child.exitCode !== null) throw new Error("null sink peak meter exited");
      return Date.now() - updatedAt <= 300 ? { state, since } : { state: null, since: null };
    },
    async stop() {
      child.kill();
      await closed;
    },
  };
}

async function pausedBaseline(app, meter) {
  const current = await app.waitFor(async (snapshot) => {
    const player = snapshot?.player;
    const page = await app.evalPlayer(PAGE_PLAYBACK_JS);
    if (!player || !page) return null;
    const videoId = player.currentTrack?.videoId ?? null;
    if (videoId && page.videoId !== videoId) return null;
    if (player.playback === "playing" && !page.paused) return { needsPause: true };
    if (["idle", "paused", "ended"].includes(player.playback) && page.paused) return { needsPause: false };
    return null;
  }, { timeoutMs: 16_000, intervalMs: 100 });
  if (!current.ok) throw new Error("player did not settle before the next playback sample");
  if (current.value.needsPause) {
    const pause = await app.clickCommand(PLAY_BUTTON, { op: "toggle" });
    if (pause.finalStatus !== "observed") throw new Error("pause was not observed before the next playback sample");
  }
  const silent = await app.waitFor(async (snapshot) => {
    const player = snapshot?.player;
    if (!player || !["paused", "idle", "ended"].includes(player.playback)) return null;
    const [page, audio] = await Promise.all([app.evalPlayer(PAGE_PLAYBACK_JS), sinkInputsFor(app.pids())]);
    const videoId = player.currentTrack?.videoId ?? null;
    const peak = meter.read();
    return page?.paused && (!videoId || page.videoId === videoId) && audio.ok && !audio.otherOnNullSink &&
      audio.entries.every((entry) => entry.sink === NULL_SINK) && peak.state === "quiet" &&
      Date.now() - peak.since >= 200 ? { videoId, playback: player.playback } : null;
  }, { timeoutMs: 8_000, intervalMs: 100 });
  if (!silent.ok) throw new Error("no paused and silent player baseline before the next playback sample");
  return silent.value;
}

/** Resolve a virtual row before timing starts. Its physical DOM may be outside the overscan window. */
async function revealSearchRow(app, index) {
  if (!await app.revealListRow(SEARCH_LIST, index)) return null;
  const row = `${SEARCH_LIST} button[data-row-index="${index}"]:enabled:not([aria-disabled="true"])`;
  return await app.exists(row) ? row : null;
}

function clickLifecycle(events, op, videoId) {
  const sent = events.find((e) => e.type === "ui->host" && e.kind === "ui.playerCommand" &&
    e.op === op && (op !== "playTarget" || e.args?.videoId === videoId));
  const toBridge = sent && events.find((e) => e.type === "core->bridge" && e.kind === "player.command" && e.commandId === sent.commandId);
  const observed = sent && events.find((e) => e.type === "core->ui" && e.receipt?.commandId === sent.commandId && e.receipt.status === "observed");
  return { sent, toBridge, observed };
}

/** Time one play action from a verified silent baseline, observing only the selected video. */
async function clickToSound(app, meter, selector, op, videoId) {
  await app.evalPlayer(PAGE_TIMING_JS);
  try {
    const since = (await app.state()).seq;
    const clickAt = Date.now();
    if (!(await app.click(selector)).clicked) throw new Error("playback control was not clickable");
    let streamAt = null;
    let advanceAt = null;
    let firstPosition = null;
    let lifecycle = {};
    while (Date.now() - clickAt < 20_000) {
      const [audio, page] = await Promise.all([
        sinkInputsFor(app.pids()),
        app.evalPlayer(PAGE_PLAYBACK_JS).catch(() => null),
      ]);
      const targetPlaying = page?.videoId === videoId && !page.paused && Number.isFinite(page.seconds);
      if (targetPlaying) {
        const sampleAt = Date.now();
        const peak = meter.read();
        if (streamAt === null && peak.state === "active" && peak.since >= clickAt && audio.ok && !audio.otherOnNullSink &&
          audio.entries.some((entry) => !entry.corked && !entry.mute) &&
          audio.entries.every((entry) => entry.sink === NULL_SINK)) {
          streamAt = peak.since;
        }
        firstPosition ??= page.seconds;
        if (advanceAt === null && page.seconds > firstPosition + 0.05) advanceAt = sampleAt;
      } else {
        firstPosition = streamAt = advanceAt = null;
      }
      if (streamAt !== null && advanceAt !== null) {
        lifecycle = clickLifecycle(await app.events(since), op, videoId);
        if (lifecycle.sent?.ack?.ok && lifecycle.toBridge && lifecycle.observed) break;
      }
      await sleep(20);
    }
    const { sent, toBridge, observed } = lifecycle.observed ? lifecycle :
      clickLifecycle(await app.events(since), op, videoId);
    const correlated = Boolean(sent?.ack?.ok && toBridge && observed && streamAt !== null && advanceAt !== null);
    const events = (await app.evalPlayer(`window.__qaPerfTiming?.events ?? []`).catch(() => []))
      .filter((event) => event.at >= clickAt);
    const playing = events.find((event) => event.t === "playing" && event.videoId === videoId);
    const loadstart = playing?.load == null ? null : events.find((event) =>
      event.t === "loadstart" && event.load === playing.load && event.at <= playing.at);
    return {
      op,
      correlated,
      ipcMs: sent ? sent.t - clickAt : null,
      coreToBridgeMs: toBridge ? toBridge.t - clickAt : null,
      observedReceiptMs: observed ? observed.t - clickAt : null,
      pagePlayingMs: playing ? Math.round(playing.at - clickAt) : null,
      networkServiceWaitMs: loadstart ? Math.round(playing.at - loadstart.at) : null,
      streamActiveMs: correlated ? streamAt - clickAt : null,
      positionAdvancingMs: correlated ? advanceAt - clickAt : null,
      mediaEvents: events.map(({ t, at }) => ({ t, ms: Math.round(at - clickAt) })),
    };
  } finally {
    await app.evalPlayer(`window.__qaPerfTiming?.controller.abort(); delete window.__qaPerfTiming;`).catch(() => {});
  }
}

async function glassScene(app) {
  const [view, windows, media, audio] = await Promise.all([
    app.evalMain(`(() => {
      const list = document.querySelector(${JSON.stringify(SEARCH_LIST)});
      const region = list?.closest('[data-testid="main-scroll-region"]');
      const covers = [...(list?.querySelectorAll('img[src^="app://covers/"]') ?? [])];
      return {
        decodedCovers: covers.filter((img) => img.complete && img.naturalWidth > 0).length,
        scrollRange: region ? region.scrollHeight - region.clientHeight : 0,
        glassFilter: getComputedStyle(document.querySelector('[data-testid="player-bar"]')).backdropFilter,
      };
    })()`),
    app.call("windows"),
    app.evalPlayer(`(() => {
      const media = document.querySelector("video");
      return media ? { paused: media.paused, seconds: media.currentTime } : null;
    })()`),
    sinkInputsFor(app.pids()),
  ]);
  const player = windows.find((w) => w.role === "player");
  return {
    ...view,
    lensActive: view.glassFilter.includes("lg-lens-"),
    playerHidden: player?.visible === false,
    playerOnMusicOrigin: player?.origin === "https://music.youtube.com",
    media,
    streamOnNullSink: audio.entries.some((e) => e.sink === NULL_SINK && !e.corked),
  };
}

const isActiveGlassScene = (scene) =>
  scene.decodedCovers > 0 && scene.scrollRange > 0 && scene.lensActive &&
  scene.playerHidden && scene.playerOnMusicOrigin && scene.media?.paused === false &&
  scene.streamOnNullSink;

async function measureLiveGlass(env, out) {
  const app = await AppInstance.launch({ label: "glass-live", workDir, ...target });
  try {
    if (!(await waitAppReady(app, { timeoutMs: 90_000 })).ok) throw new Error("live glass player did not authenticate");
    await app.call("winop", { target: "main", op: "show" });
    await app.call("winop", { target: "main", op: "setSize", args: [1280, 800] });
    const search = await uiSearch(app, opt.query);
    if (!search.renderedRows) throw new Error("live glass search returned no rows");
    await app.scrollIntoView(SEARCH_LIST, "end");
    const playback = await app.clickCommand(FIRST_PLAYABLE, { op: "playTarget" });
    if (playback.finalStatus !== "observed") throw new Error("live glass playback did not become observed");
    const warm = await app.waitFor(async () => {
      const scene = await glassScene(app);
      return isActiveGlassScene(scene) && scene;
    });
    if (!warm.ok) throw new Error("live cover decode, scroll, refraction, and hidden audio did not converge");
    const refreshHz = Array.isArray(env.display) && env.display[0]?.refreshHz;
    if (!(refreshHz > 0)) throw new Error("display refresh rate could not be measured");
    const sample = await frameSample(app, { seconds: Number(opt["scroll-sec"]) });
    const after = await glassScene(app);
    const { intervals, ...frameDetails } = sample;
    const budgetMs = 1000 / refreshHz;
    const over2x = intervals.filter((v) => v > 2 * budgetMs).length / intervals.length;
    const active = isActiveGlassScene(after) && after.media.seconds > warm.value.media.seconds;
    out.glass = {
      before: warm.value,
      after,
      refreshHz,
      budgetMs: round(budgetMs, 3),
      over2xBudgetRatio: round(over2x, 4),
      ...frameDetails,
      status: active && !sample.documentHidden && sample.frames > 0 && over2x < 0.01 ? "PASS_TARGET" : "FAIL",
    };
  } finally {
    const q = await quitViaMpris(app);
    if (q.leftoverPids.length) await app.kill();
  }
}

async function sampleResources(app, playback, visible) {
  const observe = async () => {
    const windows = await app.call("windows");
    const main = windows.find((window) => window.role === "main");
    const player = windows.find((window) => window.role === "player");
    const page = await app.evalPlayer(`({ hidden: document.hidden, paused: document.querySelector("video")?.paused ?? null })`);
    return main?.visible === visible && player?.visible === false && page?.hidden === true &&
      page.paused === (playback === "paused") ? { mainVisible: visible, playerHidden: true, page } : null;
  };
  const start = await observe();
  if (!start) throw new Error(`could not start resource sample: ${playback}, main visible=${visible}`);
  const before = resourceSample(app.pids(), app.cgroups());
  await sleep(Number(opt["paused-sec"]) * 1000);
  const after = resourceSample(app.pids(), app.cgroups());
  const end = await observe();
  if (!end) throw new Error(`resource sample changed state: ${playback}, main visible=${visible}`);
  return { ...resourceDelta(before, after), start, end };
}

async function measurePerformance(env, out) {
  // 1) starts
  const starts = [];
  for (let i = 0; i < Number(opt.starts) + 1; i++) starts.push(await startToInteractive(i));
  out.startToInteractive = {
    firstLaunch: starts[0],
    warm: stats(starts.slice(1).map((s) => s.interactiveMs).filter(Number.isFinite)),
    warmBridgeReady: stats(starts.slice(1).map((s) => s.bridgeReadyMs).filter(Number.isFinite)),
    raw: starts,
    note: "first launch of this series is separate; it is not a cache-dropped cold start",
  };

  // 2..4) one long-lived instance
  const app = await AppInstance.launch({ label: "perf-main", workDir, ...target });
  let meter;
  try {
    if (!(await waitAppReady(app, { timeoutMs: 90_000 })).ok) throw new Error("performance player did not authenticate");
    await app.call("winop", { target: "main", op: "show" });
    await app.call("winop", { target: "main", op: "setSize", args: [1280, 800] });
    const search = await uiSearch(app, opt.query);
    out.notes.push({ searchItems: search.response?.itemCount ?? null });
    const searchIds = search.response?.ids ?? [];
    const usedTargets = new Set();
    let nextRow = 0;
    let lastTarget = null;
    const clicks = [];
    const n = Number(opt.clicks);
    out.clickToSound = { plannedCount: n, raw: clicks };
    meter = await startPeakMeter();
    for (let i = 0; i < n; i++) {
      const baseline = await pausedBaseline(app, meter);
      if (i % 2 === 0) {
        // A distinct track for each new-target trial, excluding the track restored on launch.
        let selector = null;
        let videoId = null;
        let index;
        while (nextRow < searchIds.length && !selector) {
          index = nextRow++;
          videoId = searchIds[index];
          if (videoId && videoId !== baseline.videoId && !usedTargets.has(videoId)) {
            selector = await revealSearchRow(app, index);
          }
        }
        if (!selector) throw new Error("not enough distinct playable search results for the requested target samples");
        usedTargets.add(videoId);
        lastTarget = videoId;
        clicks.push({ kind: "playTarget", rowIndex: index, ...(await clickToSound(app, meter, selector, "playTarget", videoId)) });
      } else {
        if (baseline.playback !== "paused" || baseline.videoId !== lastTarget) {
          throw new Error("resume did not start from the paused target track");
        }
        await sleep(2500);
        clicks.push({ kind: "resume", ...(await clickToSound(app, meter, PLAY_BUTTON, "toggle", lastTarget)) });
      }
      await sleep(3000);
    }
    await meter.stop();
    meter = undefined;
    for (const kind of ["playTarget", "resume", "combined"]) {
      const valid = clicks.filter((c) => c.correlated && (kind === "combined" || c.kind === kind));
      out.clickToSound[kind] = Object.fromEntries(
        ["ipcMs", "coreToBridgeMs", "observedReceiptMs", "pagePlayingMs", "networkServiceWaitMs", "streamActiveMs", "positionAdvancingMs"]
          .map((key) => [key, stats(valid.map((c) => c[key]).filter(Number.isFinite))]),
      );
    }
    out.clickToSound.note = "each sample starts with a quiet 200 ms null-sink peak envelope and requires the exact video, its observed receipt, advancing playback and active peak envelope at 25 Hz; network/service wait requires a matched media load";

    // 3) 60 s scroll in the real UI
    const refresh = env.display?.[0]?.refreshHz;
    if (!(refresh > 0)) throw new Error("display refresh rate could not be measured");
    await app.scrollIntoView(SEARCH_LIST, "end");
    const fs = await frameSample(app, { seconds: Number(opt["scroll-sec"]) });
    const budget = 1000 / refresh;
    const over2xBudget = fs.intervals.filter((v) => v > 2 * budget).length / fs.intervals.length;
    const { intervals, ...frameDetails } = fs;
    out.scrollFrames = { route: "search", refreshHz: refresh, budgetMs: round(budget, 2),
      ...frameDetails, over2xBudget: round(over2xBudget, 4) };

    // 4) The steady-state process-tree samples keep the same player and profile.
    await sleep(5000);
    out.visiblePlaying = await sampleResources(app, "playing", true);
    await app.call("winop", { target: "main", op: "hide" });
    await sleep(5000);
    out.hiddenPlaying = await sampleResources(app, "playing", false);
    await app.call("winop", { target: "main", op: "show" });
    const pause = await app.clickCommand('[data-testid="btn-play-pause"]', { op: "toggle" });
    if (pause.finalStatus !== "observed") throw new Error("pause was not observed before sampling");
    await app.call("winop", { target: "main", op: "hide" });
    await sleep(5000);
    out.hiddenPaused = await sampleResources(app, "paused", false);
    out.completeSamples = out.startToInteractive.warm.n === Number(opt.starts) && fs.scrollable && fs.scrollDistancePx > 0 && !fs.documentHidden &&
      clicks.length === n && clicks.every((click) => click.correlated);
    const warm = out.startToInteractive.warm;
    const sound = out.clickToSound.combined.streamActiveMs;
    const fullRun = Number(opt.starts) === 20 && n === 20 &&
      Number(opt["scroll-sec"]) === 60 && Number(opt["paused-sec"]) === 60;
    out.criteria = {
      fullRun,
      startup: warm.n === 20 && warm.p50 <= 1000 && warm.p95 <= 1500,
      clickToSound: sound.n === 20 && sound.p50 <= 5000 && sound.p95 <= 8000,
      scroll: Number(opt["scroll-sec"]) === 60 && fs.scrollDistancePx > 0 && fs.frames > 0 &&
        !fs.documentHidden && over2xBudget < 0.01,
      hiddenPausedCpu: Number(opt["paused-sec"]) === 60 && out.hiddenPaused.cpuPctOfOneCore < 1,
      hiddenPausedPssAdvisory: out.hiddenPaused.pssMb < 800,
    };
    if (fullRun && opt.target === "installed") {
      const { startup, clickToSound: soundPass, scroll, hiddenPausedCpu } = out.criteria;
      out.status = out.completeSamples && startup && soundPass && scroll && hiddenPausedCpu ? "PASS_TARGET" : "FAIL";
    }
  } finally {
    await meter?.stop();
    const q = await quitViaMpris(app);
    if (q.leftoverPids.length) await app.kill();
  }
}

async function main() {
  mkdirSync(workDir, { recursive: true });
  const sink = await ensureNullSink();
  const out = {
    target: opt.target,
    daemonSha256: fileSha256(target.daemonBin),
    mainSha256: fileSha256(target.extraEnv.YTMCP_QA_MAIN),
    startedAt: new Date().toISOString(),
    notes: [],
  };
  try {
    out.electronSha256 = fileSha256(ELECTRON);
    if (opt.target === "installed") {
      out.installedElectronSha256 = fileSha256(INSTALLED_ELECTRON);
      if (!out.installedElectronSha256 || out.electronSha256 !== out.installedElectronSha256) {
        throw new Error("QA Electron must match the installed runtime byte for byte");
      }
    }
    const env = await collectEnvironment();
    out.env = env;
    if (opt["glass-only"]) {
      await measureLiveGlass(env, out);
    } else {
      await measurePerformance(env, out);
    }
  } catch (error) {
    out.error = error.message;
    process.exitCode = 1;
  } finally {
    await unloadNullSink(sink.created ? sink.moduleId : null);
  }
  out.finishedAt = new Date().toISOString();
  const dir = join(REPO, "artifacts/live/perf");
  mkdirSync(dir, { recursive: true });
  const mode = opt["glass-only"] ? "glass-live" : "perf";
  const file = join(dir, `${mode}-${opt.target}-${out.startedAt.replace(/[:.]/g, "-")}.json`);
  writeFileSync(file, `${JSON.stringify(out, null, 2)}\n`);
  if (out.error) {
    console.error(out.error);
    console.log(file);
    return;
  }
  if (opt["glass-only"]) {
    console.log(JSON.stringify(out.glass, null, 2));
    console.log(file);
    if (out.glass.status !== "PASS_TARGET") process.exitCode = 1;
    return;
  }
  console.log(JSON.stringify({ startToInteractive: out.startToInteractive.warm, clickToSound: { playTarget: out.clickToSound.playTarget.streamActiveMs, resume: out.clickToSound.resume.streamActiveMs, combined: out.clickToSound.combined.streamActiveMs }, scroll: out.scrollFrames, hiddenPaused: { cpu: out.hiddenPaused.cpuPctOfOneCore, wakeups: out.hiddenPaused.wakeupsPerSec, pssMb: out.hiddenPaused.pssMb } }, null, 2));
  console.log(file);
  if (!out.completeSamples || out.status === "FAIL") process.exitCode = 1;
}

await main();

