// Electron entry for `run-live-bridge-probe.mjs --entry queue-tune`: what clicking an Up Next filter
// does on the official page. It plays one public search result, clicks "Familiar" while it plays,
// queues another public track next and clicks "Discover", then pauses and clicks "Popular". It
// records whether the current media kept playing without a reload (media events, position, track
// instance), whether the suggestions changed, whether the hand-queued entry stayed, and whether a
// paused player stayed paused. With YTMCP_LIVE_TUNE_VIA=app it selects the filters through the
// app's own setQueueFilter command (core confirmation included) instead of clicking in the page.
// Only this session's play queue changes; nothing in the library. The report holds fingerprints
// and counts, no ids, titles, tokens or account data.
import { randomUUID } from "node:crypto";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { app } from "electron";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// YTMCP_LIVE_TUNE_VIA=app sends the app's setQueueFilter command instead of clicking in the page.
const VIA_APP = process.env.YTMCP_LIVE_TUNE_VIA === "app";
const playback = [];
const queues = [];
const ready = [];
const receipts = new Map();
const commandsToBridge = [];
const acks = [];
const catalog = new Map();
let latestSnapshot = null;
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


/** Page facts without ids: list lengths, fingerprints of video id lists, the selected filter. */
const PAGE_STATE = `(() => {
  const fp = (list) => { let h = 2166136261; for (const r of list) { const id = (r?.playlistPanelVideoRenderer ?? r)?.videoId ?? "?"; for (const c of id + "|") { h ^= c.charCodeAt(0); h = Math.imul(h, 16777619) >>> 0; } } return list.length + ":" + h.toString(16); };
  const bar = document.querySelector("ytmusic-player-bar");
  const q = typeof bar?.getState === "function" ? bar.getState()?.queue : null;
  const items = Array.isArray(q?.items) ? q.items : [];
  const tail = Array.isArray(q?.automixItems) ? q.automixItems : [];
  const chips = (q?.steeringChips?.chips ?? []).map((c) => c.chipCloudChipRenderer).filter(Boolean);
  const video = document.querySelector("#movie_player video");
  const player = document.getElementById("movie_player");
  const sel = items.findIndex((r) => (r?.playlistPanelVideoRenderer ?? r)?.selected === true);
  return {
    items: fp(items), tail: fp(tail), selectedIndex: q?.selectedItemIndex ?? null, selectedRow: sel,
    selectedFilter: chips.filter((c) => c.isSelected).map((c) => (c.text?.runs ?? []).map((r) => r.text).join("")),
    filters: chips.length, isFetchingChipSteer: q?.isFetchingChipSteer ?? null, isInfinite: q?.isInfinite ?? null, autoplay: q?.autoplay ?? null,
    playerState: player?.getPlayerState?.() ?? null, currentTime: video ? Math.round(video.currentTime * 1000) : null, paused: video?.paused ?? null,
    srcToken: video?.currentSrc ? (() => { let h = 0; for (const c of video.currentSrc) h = (Math.imul(h, 31) + c.charCodeAt(0)) >>> 0; return h.toString(16); })() : null,
    currentVideo: player?.getVideoData?.()?.video_id ?? null,
    // Does the row the queue calls selected carry the video the player is playing?
    selectedMatchesPlayer: (() => { const row = items[q?.selectedItemIndex ?? -1]; const id = (row?.playlistPanelVideoRenderer ?? row)?.videoId ?? null; return id === null ? null : id === (player?.getVideoData?.()?.video_id ?? null); })(),
    selectedRowKind: (() => { const row = items[q?.selectedItemIndex ?? -1]; return row ? Object.keys(row)[0] ?? null : null; })(),
    playerInTail: tail.findIndex((r) => (r?.playlistPanelVideoRenderer ?? r)?.videoId === (player?.getVideoData?.()?.video_id ?? "-")),
    events: (window.__ytmcpProbeEvents ?? []).splice(0),
  };
})()`;

const WATCH_MEDIA = `(() => {
  const video = document.querySelector("#movie_player video");
  window.__ytmcpProbeEvents = window.__ytmcpProbeEvents ?? [];
  if (video && !video.__ytmcpProbe) {
    video.__ytmcpProbe = true;
    for (const type of ["emptied", "loadstart", "abort", "seeking", "seeked", "waiting", "pause", "play", "playing", "ended", "durationchange"]) {
      video.addEventListener(type, () => window.__ytmcpProbeEvents.push([type, Math.round(performance.now()), Math.round(video.currentTime * 1000)]));
    }
  }
  return Boolean(video);
})()`;

/** Clicks the named Up Next filter the way a listener would. */
const clickFilter = (label) => `(() => {
  const want = ${JSON.stringify(label)};
  const chips = [...document.querySelectorAll("ytmusic-player-queue ytmusic-chip-cloud-chip-renderer, ytmusic-player-page ytmusic-chip-cloud-chip-renderer")]
    .filter((c) => (c.textContent ?? "").trim() === want);
  const chip = chips[0];
  if (!chip) return { clicked: false, candidates: 0 };
  const target = chip.querySelector("a, button, [role='button'], yt-formatted-string") ?? chip;
  target.click();
  return { clicked: true, candidates: chips.length, target: target.tagName.toLowerCase() };
})()`;

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
        else if (env.kind === "core.delta" && p?.deltaType === "capabilitiesChanged" && latestSnapshot) latestSnapshot.capabilities = p.capabilities;
        else if (env.kind === "core.commandReceipt") receipts.set(p.commandId, p);
        else if ((env.kind === "core.catalogResponse" || env.kind === "core.error") && env.requestId) catalog.set(env.requestId, env);
        notify();
      },
    },
  });
  const forward = integrated.hooks.onBridgeEnvelope;
  integrated.hooks.onBridgeEnvelope = async (env) => {
    if (env.kind === "bridge.ready") ready.push(env.payload);
    if (env.kind === "bridge.playbackObserved") playback.push({ ...env.payload, _t: Date.now() });
    if (env.kind === "bridge.queueObserved") queues.push({ ...env.payload, _t: Date.now() });
    if (env.kind === "bridge.commandAck") acks.push({ t: Date.now(), dispatched: env.payload?.dispatched ?? null, error: env.payload?.error?.code ?? null });
    await forward?.(env);
    notify();
  };
  const controller = await startDesktopShell(integrated.hooks);
  if (!controller) throw new Error("single-instance lock not acquired");
  // Every command the core sends to the page bridge, with the few arguments that explain it.
  const t0Run = Date.now();
  const toBridge = controller.windowManager.sendToPlayerBridge.bind(controller.windowManager);
  controller.windowManager.sendToPlayerBridge = (env) => {
    if (env.kind === "player.command") {
      const a = env.payload?.args ?? {};
      commandsToBridge.push({
        t: Date.now() - t0Run, op: env.payload?.op ?? null, ours: String(env.payload?.commandId ?? "").startsWith("live-"),
        positionMs: a.positionMs ?? null, enqueuePosition: a.enqueuePosition ?? null, hasTarget: a.target != null,
        targetHasPlaylist: a.target?.playlistId != null, hasQueueEntry: a.queueEntryId != null, autoplay: a.autoplay ?? null,
      });
    }
    return toBridge(env);
  };
  app.on("will-quit", () => void integrated.shutdown());
  await integrated.attachController(controller);
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
  const command = async (op, args, effect, timeoutMs = 30_000) => {
    const commandId = `live-${op}-${randomUUID()}`;
    send("ui.playerCommand", { commandId, op, args: args ?? null });
    await waitFor(() => ["observed", "failed", "indeterminate", "cancelled"].includes(receipts.get(commandId)?.status)
      && (!effect || effect(playback.at(-1))), timeoutMs);
    return receipts.get(commandId)?.status ?? "probe_timeout";
  };
  const js = (code) => controller.windowManager.getPlayerWindow()?.webContents.executeJavaScript(code, true)
    .catch((err) => ({ error: String(err?.message ?? err) }));
  const bridgeNow = (t0) => {
    const p = playback.at(-1);
    const q = queues.at(-1);
    return {
      instance: p?.trackInstanceId ?? null, playback: p?.playback ?? null, positionMs: p?.positionMs ?? null,
      instanceChanges: new Set(playback.filter((x) => x._t >= t0).map((x) => x.trackInstanceId)).size,
      queueRevision: q?.queueRevision ?? null, entries: q?.entries?.length ?? null,
      currentIndex: q?.entries?.findIndex((e) => e.entryId === q.currentEntryId) ?? null,
      hasCurrentId: q?.currentEntryId != null,
      playbackEntryInWindow: q?.entries?.some((e) => e.entryId === p?.currentQueueEntryId) ?? false,
      playbackHasEntryId: p?.currentQueueEntryId != null,
      // How the anchor fared in every queue snapshot since t0: [ms, revision, entries, anchor index].
      anchorTrail: queues.filter((x) => x._t >= t0 - 30_000).slice(-12).map((x) => [x._t - t0, x.queueRevision, x.entries?.length ?? 0, x.entries?.findIndex((e) => e.entryId === x.currentEntryId) ?? -1, x.currentEntryId != null]),
      // What the new bridge reports, so a failure can be placed in the bridge or the core.
      official: q?.officialUpcoming ? { status: q.officialUpcoming.status, tracks: q.officialUpcoming.tracks?.length ?? 0 } : null,
      filters: (q?.filters ?? []).map((f) => [f.filterId, f.label, f.selected]),
      audio: p ? [p.codec ?? null, p.audioItag ?? null, p.audioQuality ?? null, p.audioBitrateKbps ?? null] : null,
      queueEnqueueCapability: latestSnapshot?.capabilities?.queueEnqueue ?? ready.at(-1)?.capabilities?.queueEnqueue ?? null,
      queueFilterCapability: latestSnapshot?.capabilities?.queueFilter ?? ready.at(-1)?.capabilities?.queueFilter ?? null,
    };
  };
  /** Clicks a filter and samples page, media events and bridge facts afterwards. */
  const tune = async (label, seed, manualEntryId) => {
    await js(WATCH_MEDIA);
    const t0 = Date.now();
    const before = { page: await js(PAGE_STATE), bridge: bridgeNow(t0) };
    const beforeInstance = before.bridge.instance;
    let click;
    if (VIA_APP) {
      // The app's own path: ui.playerCommand setQueueFilter with the id from the bridge's snapshot.
      const filter = (queues.at(-1)?.filters ?? []).find((f) => f.label === label);
      const started = Date.now();
      const status = filter ? await command("setQueueFilter", { queueFilterId: filter.filterId }, null, 15_000) : "no_such_filter";
      click = { via: "app", status, filterOffered: Boolean(filter), latencyMs: Date.now() - started };
    } else {
      click = await js(clickFilter(label));
    }
    const samples = [];
    for (const at of [1_000, 3_000, 6_000, 10_000]) {
      await sleep(at - (samples.at(-1)?.atMs ?? 0));
      const page = await js(PAGE_STATE);
      const q = queues.at(-1);
      samples.push({
        atMs: at, page, bridge: bridgeNow(t0),
        sameInstance: playback.at(-1)?.trackInstanceId === beforeInstance,
        stillSeed: page?.currentVideo === seed,
        manualEntryKept: manualEntryId ? Boolean(q?.entries?.some((e) => e.entryId === manualEntryId)) : null,
      });
    }
    return { label, click, before, samples };
  };

  const report = { probe: "queue tune (clicks Up Next filters; queue only)", startedAt: new Date().toISOString(), electron: process.versions.electron, steps: [] };
  try {
    send("ui.subscribeSnapshot", {});
    if (!(await waitFor(() => ready.some((r) => r.loggedInObserved), 45_000))) throw new Error("bridge.ready(loggedInObserved) not seen");
    await waitFor(() => latestSnapshot?.authState?.state === "authenticated", 30_000);
    const search = await query({ kind: "searchSongs", query: "Daft Punk", limit: 10 });
    const songs = (search?.payload?.songs?.items ?? []).filter((s) => s.availability === "available");
    if (songs.length < 2) throw new Error("public search returned too few songs");
    const seed = songs[0].videoId;
    report.steps.push({ play: await command("playTarget", { target: { videoId: seed } }, (p) => p?.currentTrack?.videoId === seed && p.playback === "playing") });
    await waitFor(() => (playback.at(-1)?.positionMs ?? 0) >= 20_000, 40_000);

    // 1. Playing, nothing queued by hand.
    report.steps.push(await tune("Familiar", seed, null));

    // 2. A track the listener queued next, then another filter.
    const extra = songs.find((s) => s.videoId !== seed)?.videoId;
    const beforeIds = new Set((queues.at(-1)?.entries ?? []).map((e) => e.entryId));
    report.steps.push({ enqueueNext: await command("enqueue", { target: { videoId: extra }, enqueuePosition: "next" }, null, 20_000) });
    await waitFor(() => (queues.at(-1)?.entries ?? []).some((e) => !beforeIds.has(e.entryId) && e.videoId === extra), 10_000);
    const manual = (queues.at(-1)?.entries ?? []).find((e) => !beforeIds.has(e.entryId) && e.videoId === extra);
    report.manualQueued = Boolean(manual);
    report.steps.push(await tune("Discover", seed, manual?.entryId ?? null));

    // 3. Paused: a filter change must not start playback.
    report.steps.push({ pause: await command("pause", null, (p) => p?.playback === "paused", 10_000) });
    report.steps.push(await tune("Popular", seed, manual?.entryId ?? null));
    report.endPlayback = playback.at(-1)?.playback ?? null;
    if (report.endPlayback !== "paused") report.steps.push({ pauseAgain: await command("pause", null, (p) => p?.playback === "paused", 10_000) });
  } catch (err) {
    report.fatal = String(err?.message ?? err);
  }
  report.commandsToBridge = commandsToBridge;
  report.acks = acks.map((a) => [a.dispatched, a.error]);
  report.queueTrail = queues.map((q) => [q._t, q.queueRevision, q.entries?.length ?? 0, q.entries?.findIndex((e) => e.entryId === q.currentEntryId) ?? -1, q.officialUpcoming?.status ?? null, q.officialUpcoming?.tracks?.length ?? 0]);
  report.playbackTrail = playback.filter((p, i, all) => i === 0 || p.trackInstanceId !== all[i - 1].trackInstanceId || p.playback !== all[i - 1].playback).map((p) => [p._t, p.playback, p.positionMs, (p.trackInstanceId ?? "").slice(-3), p.currentQueueEntryId != null]);
  report.finishedAt = new Date().toISOString();
  writeFileSync(process.env.YTMCP_LIVE_REPORT, `${JSON.stringify(report, null, 2)}\n`);
  app.exit(report.fatal ? 1 : 0);
}

run().catch((err) => {
  writeFileSync(process.env.YTMCP_LIVE_REPORT, `${JSON.stringify({ fatal: String(err?.stack ?? err) }, null, 2)}\n`);
  app.exit(1);
});
