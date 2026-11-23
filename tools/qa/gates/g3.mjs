// G3: playback controls through the real UI (with observed receipts and independent page
// checks), the hidden soak with workspace switch / per-app network cut / optional screen
// lock, UI reopen from snapshot, paused-hidden resource drop and honest codec facts.
import { check } from "../lib/evidence.mjs";
import {
  createVirtualDesktop,
  cutNetwork,
  onCleanup,
  removeVirtualDesktop,
  resourceDelta,
  resourceSample,
  restoreNetwork,
  round,
  run,
  setCurrentDesktop,
  sinkInputsFor,
  sleep,
  virtualDesktops,
  NULL_SINK,
} from "../lib/sys.mjs";
import { execFileSync } from "node:child_process";
import { corePlayer, navigate, pageMedia, uiSearch, waitCatalog } from "./common.mjs";

const SEL = {
  playPause: '[data-testid="btn-play-pause"]',
  next: '[data-testid="btn-next"]',
  prev: '[data-testid="btn-prev"]',
  seek: '[data-testid="player-seek-slider"]',
  volume: '[data-testid="player-volume-slider"]',
  mute: '[data-testid="btn-mute"]',
  unmute: '[data-testid="btn-mute"]',
  firstSearchRow: '[data-testid="view-search"] [data-testid="virtual-track-row"] button[data-row-index="0"]',
};

async function pageAdvances(app, ms = 2500) {
  const a = await pageMedia(app);
  await sleep(ms);
  const b = await pageMedia(app);
  return { a, b, advancedSec: a.present && b.present && a.videoId === b.videoId ? round(b.t - a.t, 2) : null, sameTrack: a.videoId === b.videoId };
}

function cmdCheck(id, what, res, effectOk, effect) {
  return check(id, what, {
    measured: { ...res, pageEffect: effect },
    pass: res.sent === true && res.ipcAck?.ok === true && res.finalStatus === "observed" && effectOk === true,
  });
}

/** Starts playback from a search result and exercises every basic control through UI clicks. */
export async function controlChecks(app, { query }) {
  const checks = [];
  const log = [];
  const search = await uiSearch(app, query);
  log.push({ step: "search", search: { querySent: search.querySent, items: search.response?.itemCount ?? null, rows: search.renderedRows } });

  // play a search result
  let r = await app.clickCommand(SEL.firstSearchRow, { op: "playTarget", timeoutMs: 25_000 });
  const target = r.args?.videoId ?? null;
  await sleep(1500);
  let eff = await pageAdvances(app);
  checks.push(cmdCheck("G3.playTarget", "click a search result -> playTarget observed; page plays that video", r, eff.b.videoId === target && eff.b.paused === false && eff.advancedSec > 1, { target, page: eff.b.videoId, advancedSec: eff.advancedSec }));
  log.push({ step: "playTarget", r, eff });

  // pause / play (the play button sends toggle)
  r = await app.clickCommand(SEL.playPause, { op: "toggle" });
  await sleep(800);
  eff = await pageAdvances(app, 1500);
  checks.push(cmdCheck("G3.pause", "play/pause button while playing -> page paused and position frozen", r, eff.b.paused === true && Math.abs(eff.advancedSec ?? 9) < 0.3, { paused: eff.b.paused, advancedSec: eff.advancedSec }));
  r = await app.clickCommand(SEL.playPause, { op: "toggle" });
  await sleep(800);
  eff = await pageAdvances(app);
  checks.push(cmdCheck("G3.play", "play/pause button while paused -> page playing and position advancing", r, eff.b.paused === false && eff.advancedSec > 1, { paused: eff.b.paused, advancedSec: eff.advancedSec }));

  // seek
  const dur = (await pageMedia(app)).d ?? 180;
  const seekToMs = Math.round(Math.min(60, dur / 2) * 1000);
  r = await app.clickCommand(SEL.seek, { op: "seek", setValue: seekToMs });
  await sleep(700);
  const afterSeek = await pageMedia(app);
  checks.push(cmdCheck("G3.seek", "seek slider -> page currentTime near the target", r, Math.abs(afterSeek.t * 1000 - seekToMs) < 4000, { targetMs: seekToMs, pageMs: Math.round(afterSeek.t * 1000) }));

  // volume, then restore
  const vol0 = Math.round(((await app.snapshot())?.player?.volume ?? 1) * 100);
  r = await app.clickCommand(SEL.volume, { op: "setVolume", setValue: 35, change: false });
  await sleep(700);
  const vMid = await pageMedia(app);
  const coreVol = (await app.snapshot())?.player?.volume;
  checks.push(cmdCheck("G3.volume", "volume slider -> page volume and core volume ~35%", r, Math.abs((vMid.apiVolume ?? vMid.elVolume * 100) - 35) <= 3 && Math.abs(coreVol * 100 - 35) <= 3, { pageApiVolume: vMid.apiVolume, pageElVolume: vMid.elVolume, coreVolume: coreVol, restoredTo: vol0 }));
  await app.clickCommand(SEL.volume, { op: "setVolume", setValue: vol0 });

  // mute / unmute
  r = await app.clickCommand(SEL.mute, { op: "setMuted" });
  await sleep(600);
  const mMid = await pageMedia(app);
  const coreMuted = (await app.snapshot())?.player?.muted;
  checks.push(cmdCheck("G3.mute", "mute button -> page muted and core muted", r, (mMid.apiMuted === true || mMid.elMuted === true) && coreMuted === true, { pageApiMuted: mMid.apiMuted, pageElMuted: mMid.elMuted, coreMuted }));
  const ru = await app.clickCommand(SEL.unmute, { op: "setMuted" });
  await sleep(600);
  const mEnd = await pageMedia(app);
  checks.push(cmdCheck("G3.unmute", "unmute button -> page and core unmuted", ru, mEnd.apiMuted !== true && mEnd.elMuted !== true && (await app.snapshot())?.player?.muted === false, { pageApiMuted: mEnd.apiMuted, pageElMuted: mEnd.elMuted }));

  // next
  const beforeNext = await pageMedia(app);
  const ti0 = (await app.snapshot())?.player?.trackInstanceId;
  r = await app.clickCommand(SEL.next, { op: "next", timeoutMs: 20_000 });
  await sleep(2000);
  const afterNext = await pageMedia(app);
  const ti1 = (await app.snapshot())?.player?.trackInstanceId;
  checks.push(cmdCheck("G3.next", "next -> the page plays a different video; core track instance changes", r, afterNext.videoId && afterNext.videoId !== beforeNext.videoId && ti1 !== ti0, { from: beforeNext.videoId, to: afterNext.videoId, trackInstanceChanged: ti1 !== ti0 }));

  // previous with position > 3 s restarts the current track
  await app.clickCommand(SEL.seek, { op: "seek", setValue: 20_000 });
  await sleep(1500);
  const beforeRestart = await pageMedia(app);
  r = await app.clickCommand(SEL.prev, { op: "previous", timeoutMs: 20_000 });
  await sleep(1200);
  const afterRestart = await pageMedia(app);
  checks.push(cmdCheck("G3.previousRestart", "previous at >3 s restarts the same video from ~0", r, afterRestart.videoId === beforeRestart.videoId && afterRestart.t < 5, { video: afterRestart.videoId, sameVideo: afterRestart.videoId === beforeRestart.videoId, fromSec: beforeRestart.t, toSec: afterRestart.t }));

  // previous at the start goes back to the previous video
  r = await app.clickCommand(SEL.prev, { op: "previous", timeoutMs: 20_000 });
  await sleep(2000);
  const afterPrev = await pageMedia(app);
  checks.push(cmdCheck("G3.previous", "previous at <3 s returns to the previous video", r, afterPrev.videoId === beforeNext.videoId, { expected: beforeNext.videoId, got: afterPrev.videoId }));
  const uiStatus = await app.text('[data-testid="player-observed-status"]');
  return { checks, log, target, uiStatusAfter: uiStatus };
}

/** Per-track position facts from the recorded core playerChanged deltas. */
export function trackResetAnalysis(deltas) {
  const tracks = new Map();
  const order = [];
  for (const d of deltas) {
    if (!d.trackInstanceId || d.positionMs == null) continue;
    if (!tracks.has(d.trackInstanceId)) {
      tracks.set(d.trackInstanceId, { videoId: d.videoId, firstPosMs: d.positionMs, maxPosMs: d.positionMs, durationMs: d.durationMs ?? null, samples: 0 });
      order.push(d.trackInstanceId);
    }
    const t = tracks.get(d.trackInstanceId);
    t.maxPosMs = Math.max(t.maxPosMs, d.positionMs);
    t.durationMs = d.durationMs ?? t.durationMs;
    t.samples += 1;
  }
  const list = order.map((id) => ({ trackInstanceId: id, ...tracks.get(id) }));
  const overrun = list.filter((t) => t.durationMs && t.maxPosMs > t.durationMs + 2000);
  const noReset = list.slice(1).filter((t) => t.firstPosMs > 15_000);
  return { tracks: list.map((t) => ({ videoId: t.videoId, firstPosMs: t.firstPosMs, maxPosMs: t.maxPosMs, durationMs: t.durationMs, samples: t.samples })), overrun: overrun.length, noReset: noReset.length };
}

async function sample(app, phase, prevRes) {
  const pids = app.pids();
  const [snap, page, audio] = await Promise.all([app.snapshot(), pageMedia(app), sinkInputsFor(pids)]);
  const res = resourceSample(pids, app.cgroups());
  const wins = await app.call("windows").catch(() => []);
  return {
    at: new Date().toISOString(),
    atMs: Date.now(),
    phase,
    core: corePlayer(snap),
    network: snap?.networkState ?? null,
    page: page.present ? {
      present: true,
      videoId: page.videoId,
      t: page.t,
      d: page.d,
      paused: page.paused,
      bufferedAheadSec: page.bufferedAheadSec,
      readyState: page.readyState,
    } : page,
    audio: audio.entries.map((e) => ({ sink: e.sink, corked: e.corked, mute: e.mute })),
    windows: wins.map((w) => ({ role: w.role, visible: w.visible, minimized: w.minimized })),
    resource: prevRes ? resourceDelta(prevRes, res) : null,
    _res: res,
  };
}

/**
 * Runs the network cut for `seconds` on every app cgroup while sampling the real page every
 * 2 s (media time, buffer ahead, readyState) next to the core's playback projection.
 */
async function netCutPhase(app, seconds, extraCgroups, during) {
  const cgroups = [...new Set([...app.cgroups(), ...extraCgroups])];
  const before = await pageMedia(app);
  const cut = await cutNetwork(cgroups);
  if (!cut.ok) return { ok: false, error: cut.error };
  const samples = [];
  let duringResult = null;
  let restored = null;
  const t0 = Date.now();
  const take = async () => {
    const [snap, page, banner] = await Promise.all([app.snapshot(), pageMedia(app), app.exists('[data-testid="banner-offline"]').catch(() => null)]);
    samples.push({ s: round((Date.now() - t0) / 1000, 1), network: snap?.networkState ?? null, offlineBanner: banner, connection: snap?.player?.connection ?? null, playback: snap?.player?.playback ?? null, videoId: page.videoId ?? null, t: page.t ?? null, paused: page.paused ?? null, readyState: page.readyState ?? null, bufferedAheadSec: page.bufferedAheadSec ?? null });
  };
  try {
    let duringStarted = false;
    while (Date.now() - t0 < seconds * 1000) {
      await take();
      if (!duringStarted && during && Date.now() - t0 > 20_000) {
        duringStarted = true;
        duringResult = await during();
      }
      await sleep(2000);
    }
  } finally {
    restored = await restoreNetwork(cut);
  }
  // A page whose buffer ran dry retries its media requests on its own schedule; give it a
  // bounded minute and record how long it took.
  const restoredAt = Date.now();
  await sleep(5_000);
  let afterAdv = await pageAdvances(app, 3000);
  while (!(afterAdv.advancedSec > 1) && Date.now() - restoredAt < 60_000) afterAdv = await pageAdvances(app, 3000);
  const pageResumedAfterSec = afterAdv.advancedSec > 1 ? round((Date.now() - restoredAt) / 1000, 1) : null;
  const after = await app.snapshot();
  let played = 0;
  let bufferingWhilePageAdvanced = 0;
  for (let i = 1; i < samples.length; i++) {
    const a = samples[i - 1];
    const b = samples[i];
    const adv = a.videoId === b.videoId && b.t != null && a.t != null ? b.t - a.t : 0;
    if (adv > 0) played += adv;
    if (b.playback === "buffering" && adv > 0.5) bufferingWhilePageAdvanced += 1;
  }
  const firstStall = samples.find((x, i) => i > 0 && x.videoId === samples[i - 1].videoId && x.t - samples[i - 1].t < 0.5);
  return {
    ok: true,
    table: cut.table,
    cgroups: cgroups.map((c) => c.split("/").pop()),
    restored,
    bufferedAheadAtCutSec: before.bufferedAheadSec ?? null,
    pagePlayedSecDuringCut: round(played, 1),
    firstPageStallAtSec: firstStall?.s ?? null,
    bufferingReportedWhilePageAdvanced: bufferingWhilePageAdvanced,
    networkStatesSeen: [...new Set(samples.map((x) => x.network))],
    offlineReportedAtSec: samples.find((x) => x.network === "offline")?.s ?? null,
    offlineBannerSeen: samples.some((x) => x.offlineBanner === true),
    playbackStatesSeen: [...new Set(samples.map((x) => x.playback))],
    samples,
    afterRestore: { network: after?.networkState, playback: after?.player?.playback, pageAdvancedSec: afterAdv.advancedSec, pageResumedAfterSec },
    during: duringResult,
  };
}

async function lockTestSession() {
  const uid = String(process.getuid());
  const sessionId = process.env.XDG_SESSION_ID || (await run("loginctl", ["show-user", uid, "--property=Display", "--value"])).stdout.trim();
  if (!sessionId) return null;
  const res = await run("loginctl", ["show-session", sessionId, "--property=User", "--property=Active", "--property=Type", "--property=LockedHint"]);
  const props = new Map(res.stdout.trim().split("\n").map((line) => line.split("=")));
  return res.ok && props.get("User") === uid && props.get("Active") === "yes" && props.get("Type") === "wayland"
    && props.get("LockedHint") === "no" ? sessionId : null;
}

async function readLockedHint(sessionId) {
  const res = await run("loginctl", ["show-session", sessionId, "--property=LockedHint", "--value"]);
  const hint = res.stdout.trim();
  return res.ok && (hint === "yes" || hint === "no") ? hint : null;
}

/**
 * Hidden soak with main hidden, main minimized, desktop switch, per-app network cut,
 * optional screen lock and manual unlock, then UI reopen.
 */
export async function soak(app, { minutes, sampleSec = 30, lockScreen = false, onOfflineBrowse = null, note = console.log }) {
  const totalMs = minutes * 60_000;
  const t0 = Date.now();
  const since = (await app.state()).seq;
  const samples = [];
  const events = [];
  let prevRes = null;
  const mark = (name, extra = {}) => events.push({ at: new Date().toISOString(), sinceStartSec: Math.round((Date.now() - t0) / 1000), name, ...extra });

  // force a natural track transition early in the soak
  const d = (await pageMedia(app)).d;
  if (d && d > 60) {
    const r = await app.clickCommand('[data-testid="player-seek-slider"]', { op: "seek", setValue: Math.round((d - 20) * 1000) });
    mark("seekNearEnd", { finalStatus: r.finalStatus });
  }
  await app.call("winop", { target: "player", op: "hide" }).catch(() => {});
  await app.call("winop", { target: "main", op: "hide" });
  mark("mainHidden");
  let phase = "main-hidden";
  let lockedSession = null;
  let lockedAt = 0;
  let unlockPrompted = false;
  const plan = [
    { atFrac: 0.2, run: async () => { await app.call("winop", { target: "main", op: "show" }); await app.call("winop", { target: "main", op: "minimize" }); phase = "main-minimized"; mark("mainMinimized"); } },
    {
      atFrac: 0.35,
      run: async () => {
        const vd = await virtualDesktops();
        const home = vd.current;
        const tmp = await createVirtualDesktop("YTM Glass QA");
        if (!tmp) { mark("desktopCreateFailed"); return; }
        const undo = onCleanup(() => {
          execFileSync("busctl", ["--user", "set-property", "org.kde.KWin", "/VirtualDesktopManager", "org.kde.KWin.VirtualDesktopManager", "current", "s", home]);
          execFileSync("busctl", ["--user", "call", "org.kde.KWin", "/VirtualDesktopManager", "org.kde.KWin.VirtualDesktopManager", "removeDesktop", "s", tmp.id]);
        });
        try {
          await setCurrentDesktop(tmp.id);
          phase = "other-desktop";
          mark("switchedToTempDesktop", { current: (await virtualDesktops()).current === tmp.id });
          samples.push(await sample(app, phase, prevRes));
          prevRes = samples.at(-1)._res;
          await sleep(25_000);
        } finally {
          await setCurrentDesktop(home);
          await removeVirtualDesktop(tmp.id);
          undo();
          const after = await virtualDesktops();
          mark("tempDesktopRemoved", { backHome: after.current === home, desktops: after.desktops.length });
          phase = "main-minimized";
        }
      },
    },
    {
      atFrac: 0.5,
      run: async () => {
        phase = "net-cut";
        mark("netCutStart");
        const nc = await netCutPhase(app, 60, [], onOfflineBrowse);
        mark("netCutEnd", { ok: nc.ok });
        events.push({ name: "netCutResult", result: nc });
        phase = "recovered";
      },
    },
    ...(lockScreen
      ? [{ atFrac: 0.7, run: async () => {
        const sessionId = await lockTestSession();
        if (!sessionId) {
          mark("screenLocked", { ok: false, blocked: "no unlocked, active Wayland session for this user" });
          return;
        }
        note("Locking the screen to test background playback. Keep it locked for about 30 seconds, then unlock it manually.");
        const request = await run("loginctl", ["lock-session", sessionId]);
        let hint = await readLockedHint(sessionId);
        const deadline = Date.now() + 10_000;
        while (request.ok && hint === "no" && Date.now() < deadline) {
          await sleep(250);
          hint = await readLockedHint(sessionId);
        }
        mark("screenLocked", { ok: request.ok && hint === "yes", requestExitCode: request.code, lockedHint: hint });
        if (request.ok && hint === "yes") {
          lockedSession = sessionId;
          lockedAt = Date.now();
          phase = "locked";
          nextSample = Date.now();
        }
      } }]
      : []),
    {
      atFrac: 0.85,
      run: async () => {
        await app.call("winop", { target: "main", op: "restore" });
        await app.call("winop", { target: "main", op: "show" });
        const before = corePlayer(await app.snapshot());
        const snapCount = (await app.state()).snapshotCount;
        await app.call("winop", { target: "main", op: "reload" });
        const w = await app.waitFor((s, st) => st.snapshotCount > snapCount, { timeoutMs: 20_000 });
        await sleep(1500);
        const dom = await app.evalMain(`({
          title: document.querySelector('[data-testid="player-track-title"]')?.textContent?.trim() ?? null,
          status: document.querySelector('[data-testid="player-observed-status"]')?.textContent?.trim() ?? null,
          pos: document.querySelector('[data-testid="player-pos-text"]')?.textContent?.trim() ?? null
        })`);
        const after = await app.snapshot();
        events.push({
          name: "uiReopen",
          result: {
            snapshotDelivered: w.ok,
            sameTrack: after?.player?.currentTrack?.videoId === before?.videoId,
            uiShowsTrack: Boolean(dom.title) && dom.title === after?.player?.currentTrack?.title,
            uiStatus: dom.status,
            uiPos: dom.pos,
            queueLoaded: after?.queueSummary?.loadedCount ?? null,
            queueCompleteness: after?.queueSummary?.completeness ?? null,
          },
        });
        phase = "main-visible";
        mark("uiReopened");
      },
    },
  ];
  let nextSample = Date.now();
  while (Date.now() - t0 < totalMs || lockedSession || plan.some((p) => !p.done)) {
    if (lockedSession) {
      const hint = await readLockedHint(lockedSession);
      if (hint !== "yes") {
        mark(hint === "no" ? "screenUnlocked" : "screenLockObservationLost", { lockedHint: hint });
        if (hint === "no") note("Desktop unlock confirmed; continuing the gate.");
        lockedSession = null;
        phase = "post-lock";
      } else if (!unlockPrompted && Date.now() - lockedAt >= 30_000) {
        note("The screen has been locked for about 30 seconds; unlock it manually. The gate continues after unlock.");
        unlockPrompted = true;
      }
    }
    const frac = (Date.now() - t0) / totalMs;
    const due = !lockedSession && plan.find((p) => !p.done && frac >= p.atFrac);
    if (due) {
      due.done = true;
      await due.run();
      continue;
    }
    if (Date.now() >= nextSample) {
      const s = await sample(app, phase, prevRes);
      prevRes = s._res;
      samples.push(s);
      nextSample = Date.now() + sampleSec * 1000;
    }
    await sleep(1000);
  }
  const deltas = (await app.events(since, ["core->ui"])).filter((e) => e.deltaType === "playerChanged");
  return { startedAt: new Date(t0).toISOString(), elapsedSec: Math.round((Date.now() - t0) / 1000), samples: samples.map(({ _res, ...s }) => s), events, trackAnalysis: trackResetAnalysis(deltas), deltaCount: deltas.length };
}

const seconds60 = () => 60;

export function screenLockCheck({ events, samples }, { lockScreen, legacyProjection = false }) {
  const locked = events.find((e) => e.name === "screenLocked");
  const unlocked = events.find((e) => e.name === "screenUnlocked");
  const lockedAt = Date.parse(locked?.at);
  const unlockedAt = Date.parse(unlocked?.at);
  const lockedPlaybackSamples = samples.filter(({ phase, atMs, page, audio }) => {
    // The old sample projection only set media fields after a successful video probe.
    const present = page?.present === true || (legacyProjection && page?.present === undefined && Number.isInteger(page?.readyState));
    return phase === "locked" && Number.isFinite(atMs) && atMs >= lockedAt && atMs < unlockedAt
      && present && Number.isFinite(page.t) && page.paused === false
      && audio?.some((entry) => entry.sink === NULL_SINK && entry.corked === false);
  }).length;
  return check("G3.screenLock", "the Wayland session locked while playback continued and was manually unlocked before reopening the UI", {
    measured: { locked: locked ?? null, unlocked: unlocked ?? null, lockedPlaybackSamples },
    pass: locked?.ok === true && locked.lockedHint === "yes" && unlocked?.lockedHint === "no" && lockedPlaybackSamples > 0,
    blocked: locked?.blocked ?? null,
    notRun: lockScreen ? null : "dry run: screen lock is done only in the final run",
  });
}

export function soakChecks(result, { requiredMinutes, lockScreen }) {
  const s = result.samples;
  let stalls = 0;
  let probeFailures = 0;
  const stallAt = [];
  for (let i = 1; i < s.length; i++) {
    const a = s[i - 1].page;
    const b = s[i].page;
    if (a?.present === false || b?.present === false) {
      probeFailures += 1;
      continue;
    }
    if (s[i].phase === "recovered" && s[i - 1].phase !== "recovered") continue; // interval spans the planned network cut
    const dt = (s[i].atMs - s[i - 1].atMs) / 1000;
    const sameTrack = a.videoId === b.videoId;
    const progressed = sameTrack ? b.t - a.t : b.t;
    if (b.paused || progressed < dt * 0.5) {
      stalls += 1;
      stallAt.push({ at: s[i].at, phase: s[i].phase, paused: b.paused, progressedSec: round(progressed, 1), intervalSec: round(dt, 1) });
    }
  }
  const netCut = result.events.find((e) => e.name === "netCutResult")?.result ?? null;
  // The first sample after the planned cut may catch the page still reloading media when its
  // buffer ran dry during the cut (an explained stall, measured by G3.netCutBufferedPlayback).
  const exemptAfterCut = s.filter((x, i) => x.phase === "recovered" && s[i - 1]?.phase !== "recovered" && netCut?.firstPageStallAtSec != null);
  const counted = s.filter((x) => !exemptAfterCut.includes(x));
  const audioOk = counted.filter((x) => x.audio.some((a) => a.sink === NULL_SINK && !a.corked)).length;
  const reopen = result.events.find((e) => e.name === "uiReopen")?.result ?? null;
  const desktop = result.events.find((e) => e.name === "switchedToTempDesktop");
  const desktopBack = result.events.find((e) => e.name === "tempDesktopRemoved");
  const phases = [...new Set(s.map((x) => x.phase))];
  const cpu = s.filter((x) => x.resource).map((x) => ({ phase: x.phase, cpuPct: x.resource.cpuPctOfOneCore, pssMb: x.resource.pssMb, wakeups: x.resource.wakeupsPerSec }));
  return [
    check("G3.soakDuration", `continuous hidden playback for >= ${requiredMinutes} min`, {
      measured: { elapsedSec: result.elapsedSec, samples: s.length, phases },
      pass: result.elapsedSec >= requiredMinutes * 60 - 5,
    }),
    check("G3.soakAudioContinuous", "every sample: page playing and advancing (no unexplained stall), stream on the null sink not corked", {
      measured: { stalls, stallAt, probeFailures, samplesWithLiveStream: audioOk, samples: s.length, exemptAfterCut: exemptAfterCut.map((x) => ({ at: x.at, liveStream: x.audio.some((a) => a.sink === NULL_SINK && !a.corked) })) },
      pass: stalls === 0 && probeFailures === 0 && audioOk === counted.length,
    }),
    check("G3.perTrackReset", "positions never exceed duration and reset at each track change", {
      measured: result.trackAnalysis,
      pass: result.trackAnalysis.overrun === 0 && result.trackAnalysis.noReset === 0 && result.trackAnalysis.tracks.length >= 2,
      note: result.trackAnalysis.tracks.length < 2 ? "no track transition happened during the soak" : null,
    }),
    check("G3.hiddenPhases", "soak covered player hidden, main hidden and main minimized", {
      measured: phases,
      pass: ["main-hidden", "main-minimized"].every((p) => phases.includes(p)),
    }),
    check("G3.workspaceSwitch", "temporary second KDE desktop: switched to it and back, desktop removed, playback continued", {
      measured: { switched: desktop?.current ?? null, back: desktopBack ?? null },
      pass: desktop?.current === true && desktopBack?.backHome === true && desktopBack?.desktops === 1,
    }),
    screenLockCheck(result, { lockScreen }),
    check("G3.netCutOffline", "per-app network cut: networkState reports offline within ~30 s and the UI shows the offline banner", {
      measured: netCut ? { networkStatesSeen: netCut.networkStatesSeen, offlineReportedAtSec: netCut.offlineReportedAtSec, offlineBannerSeen: netCut.offlineBannerSeen, table: netCut.table, restored: netCut.restored } : null,
      pass: Boolean(netCut?.networkStatesSeen?.includes("offline") && netCut.offlineReportedAtSec <= 35 && netCut.offlineBannerSeen),
      blocked: netCut?.ok === false ? netCut.error : null,
    }),
    check("G3.netCutBufferedPlayback", "during the cut the page keeps playing what it had buffered (>= buffer ahead at cut - 3 s), and core reports buffering only while the page really stalls", {
      measured: netCut ? { bufferedAheadAtCutSec: netCut.bufferedAheadAtCutSec, pagePlayedSecDuringCut: netCut.pagePlayedSecDuringCut, firstPageStallAtSec: netCut.firstPageStallAtSec, bufferingReportedWhilePageAdvanced: netCut.bufferingReportedWhilePageAdvanced, playbackStatesSeen: netCut.playbackStatesSeen } : null,
      pass: Boolean(netCut && netCut.bufferedAheadAtCutSec != null && netCut.pagePlayedSecDuringCut >= Math.min(netCut.bufferedAheadAtCutSec - 3, 40) && netCut.pagePlayedSecDuringCut > 0 && netCut.bufferingReportedWhilePageAdvanced === 0),
      note: netCut?.bufferedAheadAtCutSec != null && netCut.bufferedAheadAtCutSec < seconds60(netCut) ? "the official page buffered less than the cut length; a stall after the buffer is expected" : null,
      blocked: netCut?.ok === false ? netCut.error : null,
    }),
    check("G3.netCutRecovery", "after restore: table removed, online again, playback continues", {
      measured: netCut ? { restored: netCut.restored, afterRestore: netCut.afterRestore } : null,
      pass: Boolean(netCut?.restored?.tableGoneAfter && netCut.afterRestore.network === "online" && netCut.afterRestore.pageAdvancedSec > 1),
      note: netCut?.afterRestore?.pageResumedAfterSec != null ? `page resumed ${netCut.afterRestore.pageResumedAfterSec} s after the table was removed` : null,
      blocked: netCut?.ok === false ? netCut.error : null,
    }),
    check("G3.uiReopen", "UI reopen (renderer reload) restores track, status and queue mirror from the core snapshot", {
      measured: reopen,
      pass: Boolean(reopen?.snapshotDelivered && reopen.sameTrack && reopen.uiShowsTrack && reopen.queueLoaded > 0),
    }),
    check("G3.soakResources", "process-tree CPU/PSS recorded per sample (from /proc deltas)", {
      measured: cpu,
      pass: cpu.length >= 2,
      support: true,
    }),
  ];
}

/** Pauses through the UI and compares hidden-playing vs hidden-paused process-tree CPU. */
export async function pausedHiddenDrop(app, seconds = 60) {
  await app.call("winop", { target: "main", op: "hide" });
  await app.call("winop", { target: "player", op: "hide" }).catch(() => {});
  await sleep(3000);
  const measure = async () => {
    const a = resourceSample(app.pids(), app.cgroups());
    await sleep(seconds * 1000);
    const b = resourceSample(app.pids(), app.cgroups());
    return resourceDelta(a, b);
  };
  const playing = await measure();
  const pause = await app.clickCommand('[data-testid="btn-play-pause"]', { op: "toggle" });
  await sleep(3000);
  const paused = await measure();
  const pageAfter = await pageMedia(app);
  const resume = await app.clickCommand('[data-testid="btn-play-pause"]', { op: "toggle" });
  const summary = (d) => ({ cpuPctOfOneCore: d.cpuPctOfOneCore, wakeupsPerSec: d.wakeupsPerSec, pssMb: d.pssMb, perRole: d.perRole, cgroups: d.cgroups });
  return {
    measured: { seconds, hiddenPlaying: summary(playing), hiddenPaused: summary(paused), pauseCommand: pause.finalStatus, pagePausedDuring: pageAfter.paused, resumeCommand: resume.finalStatus },
    check: check("G3.pausedHiddenDrop", "hidden+paused process-tree CPU is lower than hidden+playing", {
      measured: { hiddenPlayingCpu: playing.cpuPctOfOneCore, hiddenPausedCpu: paused.cpuPctOfOneCore, hiddenPlayingWakeups: playing.wakeupsPerSec, hiddenPausedWakeups: paused.wakeupsPerSec, pagePausedDuring: pageAfter.paused },
      pass: pause.finalStatus === "observed" && pageAfter.paused === true && paused.cpuPctOfOneCore < playing.cpuPctOfOneCore,
    }),
  };
}

/** Codec / audio mode / quality / DRM as observable; unobservable fields are UNKNOWN. */
export async function mediaFacts(app) {
  const snap = await app.snapshot();
  const page = await pageMedia(app);
  const u = (v) => (v == null || v === "" ? "UNKNOWN" : v);
  const core = corePlayer(snap);
  const facts = {
    core: { codec: u(core?.codec), audioMode: u(core?.audioMode), qualityLabel: u(core?.qualityLabel), drmProtected: core?.drmProtected == null ? "UNKNOWN" : core.drmProtected },
    pageProbe: {
      statsForNerdsCodecs: u(page.statsCodecs),
      playbackQuality: u(page.quality),
      emeMediaKeysAttached: page.present ? page.emeActive : "UNKNOWN",
      videoFrameSize: page.present ? `${page.videoWidth}x${page.videoHeight}` : "UNKNOWN",
    },
    note: "pageProbe values come from the official page's own media element/player object read by QA; they are not app features",
  };
  const contradiction = facts.core.drmProtected !== "UNKNOWN" && page.present && facts.core.drmProtected !== page.emeActive;
  return {
    measured: facts,
    check: check("G3.mediaFacts", "codec/audioMode/quality/DRM recorded as observed or UNKNOWN; core does not contradict the page", {
      measured: facts,
      pass: !contradiction,
    }),
  };
}

export { navigate, waitCatalog };
