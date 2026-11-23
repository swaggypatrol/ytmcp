// Electron entry for run-live-bridge-probe.mjs. Drives the real desktop shell + daemon and sends
// every command through the same core path the UI uses (ui.playerCommand). The report keeps
// only public videoIds, opaque ids, numbers and formats: no titles, account data or cookies.
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { app } from "electron";

const MODE = process.env.YTMCP_LIVE_MODE;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const HINT_RE = /^acct:[0-9a-f]{32}$/;

const bridge = { ready: [], playback: [], queue: [], health: [], acks: [], errors: [], inbound: [] };
const receipts = new Map();
const catalog = new Map();
let latestSnapshot = null;
let t0Run = Date.now();
const waiters = new Set();
const notify = () => {
  for (const w of [...waiters]) if (w.check()) { waiters.delete(w); clearTimeout(w.timer); w.resolve(true); }
};
const waitFor = (check, timeoutMs) =>
  check()
    ? Promise.resolve(true)
    : new Promise((resolve) => {
        const w = { check, resolve, timer: setTimeout(() => { waiters.delete(w); resolve(false); }, timeoutMs) };
        waiters.add(w);
      });

const lastPlayback = () => bridge.playback.at(-1) ?? null;
const lastQueue = () => bridge.queue.at(-1) ?? null;
const pb = (p) =>
  p && {
    playback: p.playback,
    positionMs: p.positionMs,
    durationMs: p.durationMs ?? null,
    videoId: p.currentTrack?.videoId ?? null,
    trackInstanceId: p.trackInstanceId ?? null,
    entryId: p.currentQueueEntryId ?? null,
    volume: p.volume,
    muted: p.muted,
    shuffle: p.shuffle ?? null,
    repeat: p.repeat ?? null,
  };

function sinkReport() {
  try {
    const sinks = Object.fromEntries(
      execFileSync("pactl", ["list", "short", "sinks"], { encoding: "utf8" })
        .trim().split("\n").map((l) => l.split("\t")).map((c) => [c[0], c[1]])
    );
    const blocks = execFileSync("pactl", ["list", "sink-inputs"], { encoding: "utf8" }).split(/(?=^Sink Input #)/m);
    return blocks
      .filter((b) => /application\.name = "YTMcp"/.test(b))
      .map((b) => ({ sink: sinks[(/^\s*Sink: (\d+)/m.exec(b) ?? [])[1]] ?? null, corked: (/^\s*Corked: (\S+)/m.exec(b) ?? [])[1] ?? null }));
  } catch (err) {
    return [{ error: String(err.message ?? err) }];
  }
}

async function run() {
  const { startDesktopShell } = await import(pathToFileURL(process.env.YTMCP_LIVE_MAIN_MODULE).href);
  const { createIntegratedDesktopHooks } = await import(pathToFileURL(process.env.YTMCP_LIVE_HOST_MODULE).href);
  let seq = 9000;
  const integrated = createIntegratedDesktopHooks({
    clientOptions: {
      onUiEnvelope: (env) => {
        const p = env.payload;
        if (env.kind === "core.snapshot") latestSnapshot = p;
        else if (env.kind === "core.delta" && p?.deltaType === "commandUpdated" && p.receipt) receipts.set(p.receipt.commandId, p.receipt);
        else if (env.kind === "core.delta" && p?.deltaType === "authChanged" && latestSnapshot) latestSnapshot.authState = p.auth_state;
        else if (env.kind === "core.commandReceipt") receipts.set(p.commandId, p);
        else if ((env.kind === "core.catalogResponse" || env.kind === "core.error") && env.requestId) catalog.set(env.requestId, env);
        notify();
      },
    },
  });
  const forward = integrated.hooks.onBridgeEnvelope;
  integrated.hooks.onBridgeEnvelope = async (env) => {
    const key = { "bridge.ready": "ready", "bridge.playbackObserved": "playback", "bridge.queueObserved": "queue", "bridge.health": "health", "bridge.commandAck": "acks", "bridge.error": "errors" }[env.kind];
    if (key) bridge[key].push({ ...env.payload, _t: Date.now() });
    await forward?.(env);
    notify();
  };

  const controller = await startDesktopShell(integrated.hooks);
  if (!controller) throw new Error("single-instance lock not acquired");
  t0Run = Date.now();
  const toBridge = controller.windowManager.sendToPlayerBridge.bind(controller.windowManager);
  controller.windowManager.sendToPlayerBridge = (env) => {
    if (env.kind === "player.command") {
      bridge.inbound.push({ t: Date.now() - t0Run, op: env.payload?.op ?? null, commandId: String(env.payload?.commandId ?? "") });
    }
    return toBridge(env);
  };
  app.on("will-quit", () => void integrated.shutdown());
  await integrated.attachController(controller);

  /** Page facts that explain an unexpected pause; names and states only, no titles or account data. */
  const pageDiag = () =>
    controller.windowManager.getPlayerWindow()?.webContents.executeJavaScript(`(() => {
      const p = document.getElementById("movie_player");
      const pr = p?.getPlayerResponse?.();
      const shown = (el) => el && el.getClientRects().length > 0 && getComputedStyle(el).display !== "none";
      const popups = [...document.querySelectorAll("ytmusic-popup-container > *, tp-yt-paper-dialog, ytmusic-you-there-renderer, yt-confirm-dialog-renderer")]
        .filter(shown).map((el) => el.tagName.toLowerCase());
      return {
        playerState: p?.getPlayerState?.() ?? null,
        playability: pr?.playabilityStatus?.status ?? null,
        visibility: document.visibilityState,
        hasFocus: document.hasFocus(),
        userActivation: navigator.userActivation ? { hasBeenActive: navigator.userActivation.hasBeenActive, isActive: navigator.userActivation.isActive } : null,
        popups: [...new Set(popups)],
        mediaPaused: document.querySelector("#movie_player video")?.paused ?? null,
      };
    })()`, true).catch((err) => ({ error: String(err?.message ?? err) })) ?? Promise.resolve(null);

  const send = (kind, payload, requestId = `live-${randomUUID()}`) => {
    seq += 1;
    integrated.rustHost.sendEnvelope({
      protocolVersion: 1, kind, coreEpoch: integrated.rustHost.getCoreEpoch(),
      accountEpoch: latestSnapshot?.accountEpoch ?? null, bridgeEpoch: latestSnapshot?.player?.bridgeEpoch ?? null,
      requestId, seq, payload,
    });
    return requestId;
  };
  const query = async (req) => {
    const id = send("ui.catalogQuery", req);
    await waitFor(() => catalog.has(id), 20_000);
    return catalog.get(id) ?? null;
  };
  /** Sends a core command and waits for its terminal receipt and the observed effect. */
  const command = async (label, op, args, effect, timeoutMs = 15_000) => {
    const commandId = `live-${op}-${randomUUID()}`;
    const t0 = Date.now();
    const before = pb(lastPlayback());
    send("ui.playerCommand", { commandId, op, args: args ?? null });
    const terminal = () => ["observed", "failed", "indeterminate", "cancelled"].includes(receipts.get(commandId)?.status);
    // The effect counts if any bridge sample after dispatch showed it, not only the last one.
    let effectSeen = effect ? false : null;
    const sawEffect = () => {
      if (effect && !effectSeen) effectSeen = bridge.playback.some((p) => p._t >= t0 && effect(p, lastQueue(), before));
      return effect ? effectSeen : true;
    };
    await waitFor(() => terminal() && sawEffect(), timeoutMs);
    if (effect && !effectSeen) await waitFor(sawEffect, 4_000);
    const r = receipts.get(commandId);
    const bridgeAcks = bridge.acks.filter((a) => a.commandId === commandId);
    return {
      label, op, startMs: t0 - t0Run,
      receipt: r ? { status: r.status, error: r.error?.code ?? null, latencyMs: r.observedAtMs ? r.observedAtMs - r.acceptedAtMs : null } : { status: "probe_timeout" },
      // Every phase the core dispatched under this commandId, and the bridge's ack for each.
      bridgeOps: bridge.inbound.filter((c) => c.commandId === commandId).map((c) => [c.t - (t0 - t0Run), c.op]),
      bridgeAcks: bridgeAcks.map((a) => ({ dispatched: a.dispatched, error: a.error?.code ?? null })),
      effectSeen,
      before,
      after: pb(lastPlayback()),
      // Bridge samples while the command was pending, to show transient states.
      trace: bridge.playback.filter((p) => p._t >= t0).slice(0, 12).map((p) => [p._t - t0, p.playback, p.positionMs, p.currentTrack?.videoId ?? null]),
      elapsedMs: Date.now() - t0,
      // An op that should keep playing but left the page paused: record why, if the page shows it.
      pageAfter: (op === "next" || op === "playTarget") && lastPlayback()?.playback !== "playing" ? await pageDiag() : undefined,
    };
  };

  const report = { probe: "T09 live bridge", mode: MODE, startedAt: new Date().toISOString(), electron: process.versions.electron, chrome: process.versions.chrome, steps: [], transitions: [] };
  try {
    send("ui.subscribeSnapshot", {});
    const readyOk = await waitFor(() => bridge.ready.some((r) => r.loggedInObserved), 45_000);
    const ready = bridge.ready.at(-1);
    report.ready = {
      seen: readyOk,
      bridgeVersion: ready?.bridgeVersion ?? null,
      loggedInObserved: ready?.loggedInObserved ?? null,
      accountHintFormat: typeof ready?.accountHint === "string" ? (HINT_RE.test(ready.accountHint) ? "acct:<32 lowercase hex>" : "INVALID") : null,
      readyCount: bridge.ready.length,
      capabilities: ready?.capabilities ?? null,
    };
    if (!readyOk) throw new Error("bridge.ready(loggedInObserved) not seen");

    await waitFor(() => latestSnapshot?.authState?.state === "authenticated", 30_000);
    let songs = [];
    for (let attempt = 0; attempt < 4 && songs.length < 2; attempt++) {
      if (attempt > 0) await sleep(3_000);
      const search = await query({ kind: "searchSongs", query: "Daft Punk", limit: 10 });
      songs = (search?.payload?.songs?.items ?? []).filter((s) => s.availability === "available");
      if (songs.length < 2) report.searchErrors = [...(report.searchErrors ?? []), search?.payload?.code ?? search?.kind ?? "timeout"];
    }
    if (songs.length < 2) throw new Error("public search returned too few songs");
    const seed = songs[0].videoId;
    report.publicSeedVideoId = seed;
    report.steps.push(
      await command("playTarget public radio", "playTarget", { target: { videoId: seed, playlistId: `RDAMVM${seed}` } },
        (p) => p?.currentTrack?.videoId === seed && p.playback === "playing", 30_000)
    );
    if (!(await waitFor(() => lastPlayback()?.playback === "playing", 8_000))) {
      report.steps.push(await command("play after playTarget", "play", null, (p) => p.playback === "playing"));
    }
    await sleep(1500);
    report.audioWhilePlaying = sinkReport();

    // Natural gapless transitions: jump near the end and let autoplay continue.
    for (let k = 0; k < 3; k++) {
      const cur = lastPlayback();
      const from = pb(cur);
      const seekTo = Math.max(0, (cur.durationMs ?? 0) - 4_000);
      const s = await command(`seek near end #${k + 1}`, "seek", { positionMs: seekTo, trackInstanceId: cur.trackInstanceId }, (p) => p.positionMs >= seekTo - 500);
      const tSeek = Date.now();
      const changed = await waitFor(() => (lastPlayback()?.currentTrack?.videoId ?? from.videoId) !== from.videoId, 25_000);
      const tChange = Date.now();
      await sleep(4_000);
      const beforeSamples = bridge.playback.filter((p) => p._t >= tSeek && p._t < tChange).map(pb);
      const afterSamples = bridge.playback.filter((p) => p._t >= tChange).map(pb);
      const to = afterSamples[0];
      const entry = lastQueue()?.entries?.find((e) => e.entryId === afterSamples.at(-1)?.entryId) ?? null;
      report.transitions.push({
        seekReceipt: s.receipt.status,
        changed,
        from: { videoId: from.videoId, durationMs: from.durationMs, lastPositionMs: beforeSamples.at(-1)?.positionMs ?? null },
        to: {
          videoId: to?.videoId ?? null,
          firstPositionMs: to?.positionMs ?? null,
          durationMs: afterSamples.at(-1)?.durationMs ?? null,
          queueEntryDurationMs: entry?.track?.durationMs ?? null,
          maxPositionMs: Math.max(...afterSamples.map((p) => p.positionMs)),
          instanceChanged: to?.trackInstanceId !== from.trackInstanceId,
        },
        samplesAfter: afterSamples.slice(0, 6).map((p) => [p.positionMs, p.durationMs]),
      });
    }

    const v0 = lastPlayback().volume;
    report.steps.push(await command("pause", "pause", null, (p) => p.playback === "paused"));
    report.steps.push(await command("play", "play", null, (p) => p.playback === "playing"));
    report.steps.push(await command("toggle (to pause)", "toggle", null, (p) => p.playback === "paused"));
    report.steps.push(await command("toggle (to play)", "toggle", null, (p) => p.playback === "playing"));
    report.steps.push(await command("seek 30s", "seek", { positionMs: 30_000, trackInstanceId: lastPlayback().trackInstanceId },
      (p, _q, b) => p.positionMs >= 29_500 && p.positionMs <= 34_000 && p.trackInstanceId === b.trackInstanceId));
    report.steps.push(await command("setVolume 0.4", "setVolume", { volume: 0.4 }, (p) => Math.abs(p.volume - 0.4) < 0.011));
    report.steps.push(await command("setVolume restore", "setVolume", { volume: v0 }, (p) => Math.abs(p.volume - v0) < 0.011));
    report.steps.push(await command("setMuted true", "setMuted", { muted: true }, (p) => p.muted === true));
    report.steps.push(await command("setMuted false", "setMuted", { muted: false }, (p) => p.muted === false));
    report.steps.push(await command("setRepeat all", "setRepeat", { repeat: "all" }, (p) => p.repeat === "all"));
    report.steps.push(await command("setRepeat one", "setRepeat", { repeat: "one" }, (p) => p.repeat === "one"));
    report.steps.push(await command("setRepeat off", "setRepeat", { repeat: "off" }, (p) => p.repeat === "off"));
    report.steps.push(await command("setShuffle true", "setShuffle", { shuffle: true }, (p) => p.shuffle === true));
    await sleep(2_500);
    report.steps.push(await command("setShuffle false", "setShuffle", { shuffle: false }, (p) => p.shuffle === false));
    await sleep(2_500);
    // Next from playback must also leave the new track playing.
    report.steps.push(await command("next", "next", null,
      (p, _q, b) => p.currentTrack?.videoId !== b.videoId && p.trackInstanceId !== b.trackInstanceId && p.playback === "playing"));
    await sleep(1_000);
    report.steps.push(await command("seek 20s before previous", "seek", { positionMs: 20_000, trackInstanceId: lastPlayback().trackInstanceId }, (p) => p.positionMs >= 19_500));
    report.steps.push(await command("previous within track (restart)", "previous", null,
      (p, _q, b) => p.currentTrack?.videoId === b.videoId && p.trackInstanceId === b.trackInstanceId && p.positionMs < 3_000));
    report.steps.push(await command("seek 0 before previous", "seek", { positionMs: 0, trackInstanceId: lastPlayback().trackInstanceId }, (p) => p.positionMs < 1_500));
    report.steps.push(await command("previous at start (previous track)", "previous", null, (p, _q, b) => p.currentTrack?.videoId !== b.videoId));
    await sleep(1_000);

    const inQueue = new Set((lastQueue()?.entries ?? []).map((e) => e.videoId));
    const extra = songs.find((s) => !inQueue.has(s.videoId))?.videoId ?? songs[1].videoId;
    report.enqueueVideoId = extra;
    const nextOf = (q) => {
      const i = (q?.entries ?? []).findIndex((e) => e.entryId === q?.currentEntryId);
      return i >= 0 ? q.entries[i + 1] ?? null : null;
    };
    const beforeIds = new Set((lastQueue()?.entries ?? []).map((e) => e.entryId));
    report.steps.push(await command("enqueue next", "enqueue", { target: { videoId: extra }, enqueuePosition: "next" },
      (_p, q) => nextOf(q)?.videoId === extra && !beforeIds.has(nextOf(q)?.entryId), 20_000));
    report.steps.push(await command("enqueue last on endless radio", "enqueue", { target: { videoId: extra }, enqueuePosition: "last" }, null));

    const q = lastQueue();
    const ci = (q?.entries ?? []).findIndex((e) => e.entryId === q?.currentEntryId);
    const pick = q?.entries?.[ci + 2] ?? null;
    report.steps.push(await command("selectQueueEntry current+2", "selectQueueEntry", { queueEntryId: pick?.entryId, queueRevision: q?.queueRevision },
      (p) => p.currentQueueEntryId === pick?.entryId && p.currentTrack?.videoId === pick?.videoId, 15_000));
    // Composite core commands: stop = pause then seek 0; next while paused = next then pause.
    if (lastPlayback()?.playback !== "playing") report.steps.push(await command("play before stop", "play", null, (p) => p.playback === "playing"));
    await sleep(3_000);
    report.steps.push(await command("stop while playing", "stop", { trackInstanceId: lastPlayback().trackInstanceId },
      (p, _q, b) => p.playback === "paused" && p.positionMs < 1_500 && p.trackInstanceId === b.trackInstanceId));
    await sleep(1_000);
    report.steps.push(await command("next while paused", "next", null, (p, _q, b) => p.currentTrack?.videoId !== b.videoId && p.playback === "paused"));
    await sleep(3_000);
    report.stillPausedAfterSkip = lastPlayback()?.playback === "paused";
    report.steps.push(await command("play after paused skip", "play", null, (p) => p.playback === "playing"));
    report.queueAtEnd = { revision: lastQueue()?.queueRevision ?? null, entries: lastQueue()?.entries?.length ?? 0, completeness: lastQueue()?.completeness ?? null, hasMore: lastQueue()?.hasMore ?? null };

    // Cadence: bridge.playbackObserved per second while playing, then while paused.
    await waitFor(() => lastPlayback()?.playback === "playing", 5_000);
    let t0 = Date.now();
    await sleep(15_000);
    const playingRate = bridge.playback.filter((p) => p._t >= t0).length / 15;
    report.steps.push(await command("pause for cadence", "pause", null, (p) => p.playback === "paused"));
    await sleep(2_000);
    t0 = Date.now();
    await sleep(36_000);
    report.cadence = {
      playingPlaybackEventsPerSec: +playingRate.toFixed(2),
      pausedPlaybackEventsIn36s: bridge.playback.filter((p) => p._t >= t0).length,
      pausedHealthEventsIn36s: bridge.health.filter((h) => h._t >= t0).length,
    };
    report.audioAtEnd = sinkReport();
    report.bridgeErrors = bridge.errors.map((e) => e.code);
    // Commands the core issued on its own (e.g. paused-skip orchestration), by dispatch result.
    report.commandsToBridge = bridge.inbound.filter((c) => !c.commandId.startsWith("live-")).map((c) => [c.t, c.op]);
    report.coreInternalAcks = bridge.acks.filter((a) => !a.commandId.startsWith("live-")).map((a) => ({ dispatched: a.dispatched, error: a.error?.code ?? null }));
    report.readyHintStable = new Set(bridge.ready.map((r) => r.accountHint)).size === 1;
  } catch (err) {
    report.fatal = String(err?.message ?? err);
  }
  report.finishedAt = new Date().toISOString();
  writeFileSync(process.env.YTMCP_LIVE_REPORT, `${JSON.stringify(report, null, 2)}\n`);
  app.exit(report.fatal ? 1 : 0);
}

run().catch((err) => {
  writeFileSync(process.env.YTMCP_LIVE_REPORT, `${JSON.stringify({ fatal: String(err?.stack ?? err) }, null, 2)}\n`);
  app.exit(1);
});
