// Electron entry for `run-live-bridge-probe.mjs --entry queue-future`: a read-only look at what
// the official page holds after the current track. It plays one public search result on its own,
// seeks near its end to let the page continue into its autoplay suggestions, optionally plays its
// public radio (YTMCP_LIVE_FUTURE_RADIO=1), and records only the structure of the page's queue state (key names,
// value types, counts), the autoplay switch and the Up Next chips (labels, selection, endpoint
// key names). Nothing is clicked on the page and nothing in the library changes. The report has
// no titles, ids, tokens, cookies or account data.
import { randomUUID } from "node:crypto";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { app } from "electron";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const playback = [];
const queues = [];
const ready = [];
const receipts = new Map();
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

/** Runs in the page's main world. Returns shapes only: strings become their length. */
const PAGE_DUMP = `(() => {
  const shape = (v, depth) => {
    if (v === null || v === undefined) return v === null ? null : "undefined";
    if (Array.isArray(v)) return depth <= 0 ? "array(" + v.length + ")" : { array: v.length, first: v.length ? shape(v[0], depth - 1) : null };
    if (typeof v === "object") {
      if (depth <= 0) return "object";
      const out = {};
      for (const k of Object.keys(v).slice(0, 48)) out[k] = shape(v[k], depth - 1);
      return out;
    }
    if (typeof v === "string") return "string(" + v.length + ")";
    if (typeof v === "boolean" || typeof v === "number") return typeof v + ":" + (typeof v === "boolean" ? v : "n");
    return typeof v;
  };
  const bar = document.querySelector("ytmusic-player-bar");
  const state = typeof bar?.getState === "function" ? bar.getState() : null;
  const queue = state?.queue ?? null;
  const element = document.querySelector("ytmusic-player-queue#queue");
  const chips = [...document.querySelectorAll("ytmusic-player-page ytmusic-chip-cloud-chip-renderer, ytmusic-player-queue ytmusic-chip-cloud-chip-renderer")];
  const automix = document.querySelector("ytmusic-player-page ytmusic-tab-renderer #automix") ?? element?.querySelector("#automix");
  const tail = Array.isArray(queue?.automixItems) ? queue.automixItems : [];
  const items = Array.isArray(queue?.items) ? queue.items : [];
  return {
    // The playing audio stream: what the player's statistics call it, and how the page's own
    // player response labels each audio format (stream number, container, bit rate, quality).
    audio: (() => {
      const p = document.getElementById("movie_player");
      const stats = p?.getStatsForNerds?.();
      const formats = p?.getPlayerResponse?.()?.streamingData?.adaptiveFormats;
      return {
        statsCodecs: typeof stats?.codecs === "string" ? stats.codecs : null,
        playbackQuality: p?.getPlaybackQuality?.() ?? null,
        hasStreamingData: Array.isArray(formats),
        audioFormats: Array.isArray(formats) ? formats.filter((f) => typeof f?.mimeType === "string" && f.mimeType.startsWith("audio/")).map((f) => ({
          itag: f.itag, mime: f.mimeType.split(";")[0], codecs: /codecs="([^"]+)"/.exec(f.mimeType)?.[1] ?? null, bitrate: f.bitrate ?? null, averageBitrate: f.averageBitrate ?? null,
          audioQuality: f.audioQuality ?? null, isDrc: f.isDrc ?? null, audioSampleRate: f.audioSampleRate ?? null, keys: Object.keys(f).filter((k) => !["url", "signatureCipher", "cipher"].includes(k)),
        })) : null,
      };
    })(),
    queueKeys: queue ? Object.fromEntries(Object.keys(queue).map((k) => [k, Array.isArray(queue[k]) ? "array(" + queue[k].length + ")" : typeof queue[k] === "object" && queue[k] !== null ? "object" : typeof queue[k] === "string" ? "string" : typeof queue[k] + ":" + queue[k]])) : null,
    itemCount: items.length,
    automixCount: tail.length,
    firstItemShape: shape(items[0], 6),
    firstAutomixShape: shape(tail[0], 6),
    lastAutomixShape: tail.length > 1 ? shape(tail[tail.length - 1], 3) : null,
    responsesShape: shape(queue?.responses ?? queue?.response ?? null, 3),
    steeringChipsShape: shape(queue?.steeringChips ?? null, 8),
    headerShape: shape(queue?.header ?? null, 4),
    isGenerating: queue?.isGenerating ?? null,
    selectedItemIndex: queue?.selectedItemIndex ?? null,
    pendingAutomixFetch: element?.queue?.pendingFetchAutomixItemsRequest != null,
    currentInAutomix: tail.findIndex((r) => (r.playlistPanelVideoRenderer ?? r)?.selected === true),
    currentInItems: items.findIndex((r) => (r.playlistPanelVideoRenderer ?? r)?.selected === true),
    elementQueueKeys: element?.queue ? Object.keys(element.queue).slice(0, 40) : null,
    elementAutoPlaying: element?.queue?.autoPlaying ?? null,
    hasContinuation: typeof element?.queue?.continuation === "string",
    automixSwitch: automix ? { checked: automix.checked ?? null, ariaChecked: automix.getAttribute("aria-checked"), disabled: automix.matches(":disabled, [disabled], [aria-disabled='true']") } : null,
    chips: chips.map((chip) => ({
      label: (chip.textContent ?? "").trim().slice(0, 40),
      selected: chip.hasAttribute("is-selected") || chip.getAttribute("aria-selected") === "true" || chip.data?.isSelected === true,
      dataShape: shape(chip.data, 7),
    })),
    chipContainers: [...new Set(chips.map((chip) => chip.closest("ytmusic-player-queue, ytmusic-tab-renderer, ytmusic-player-page")?.tagName.toLowerCase() ?? null))],
  };
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
        else if (env.kind === "core.commandReceipt") receipts.set(p.commandId, p);
        else if ((env.kind === "core.catalogResponse" || env.kind === "core.error") && env.requestId) catalog.set(env.requestId, env);
        notify();
      },
    },
  });
  const forward = integrated.hooks.onBridgeEnvelope;
  integrated.hooks.onBridgeEnvelope = async (env) => {
    if (env.kind === "bridge.ready") ready.push(env.payload);
    if (env.kind === "bridge.playbackObserved") playback.push(env.payload);
    if (env.kind === "bridge.queueObserved") queues.push(env.payload);
    await forward?.(env);
    notify();
  };
  const controller = await startDesktopShell(integrated.hooks);
  if (!controller) throw new Error("single-instance lock not acquired");
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
  const dump = () => controller.windowManager.getPlayerWindow()?.webContents.executeJavaScript(PAGE_DUMP, true)
    .catch((err) => ({ error: String(err?.message ?? err) }));
  /** What the bridge reported for the queue: counts and flags only. */
  const bridgeQueue = () => {
    const q = queues.at(-1);
    return q && {
      entries: q.entries?.length ?? 0,
      currentInWindow: q.entries?.some((e) => e.entryId === q.currentEntryId) ?? false,
      currentIndex: q.entries?.findIndex((e) => e.entryId === q.currentEntryId) ?? -1,
      // Where a missing anchor comes from: no current id at all, or one that names no loaded entry.
      hasCurrentId: q.currentEntryId != null,
      playbackEntryInWindow: q.entries?.some((e) => e.entryId === playback.at(-1)?.currentQueueEntryId) ?? false,
      playbackHasEntryId: playback.at(-1)?.currentQueueEntryId != null,
      queueRevision: q.queueRevision,
      official: q.officialUpcoming ? [q.officialUpcoming.status, q.officialUpcoming.tracks?.length ?? 0] : null,
      loadedRange: q.loadedRange ?? null,
      completeness: q.completeness, hasMore: q.hasMore ?? null,
      explicitComplete: q.explicitComplete ?? null, mayLoadMore: q.mayLoadMore ?? null,
    };
  };
  /** Samples the page a few times, since the tail and chips load after the track starts. */
  const observe = async (label) => {
    const samples = [];
    for (const at of [3_000, 8_000, 15_000]) {
      await sleep(at - (samples.at(-1)?.atMs ?? 0));
      samples.push({ atMs: at, page: await dump(), bridge: bridgeQueue(), capabilities: ready.at(-1)?.capabilities ? { queueClear: latestSnapshot?.capabilities?.queueClear ?? null } : null, autoplay: playback.at(-1)?.autoplay ?? null });
    }
    return { label, samples };
  };

  const report = { probe: "queue future (read-only)", startedAt: new Date().toISOString(), electron: process.versions.electron, steps: [] };
  try {
    send("ui.subscribeSnapshot", {});
    if (!(await waitFor(() => ready.some((r) => r.loggedInObserved), 45_000))) throw new Error("bridge.ready(loggedInObserved) not seen");
    await waitFor(() => latestSnapshot?.authState?.state === "authenticated", 30_000);
    report.initialAutoplay = playback.at(-1)?.autoplay ?? null;
    const search = await query({ kind: "searchSongs", query: "Daft Punk", limit: 10 });
    const songs = (search?.payload?.songs?.items ?? []).filter((s) => s.availability === "available");
    if (songs.length < 1) throw new Error("public search returned no song");
    const seed = songs[0].videoId;

    report.steps.push({ play: await command("playTarget", { target: { videoId: seed } }, (p) => p?.currentTrack?.videoId === seed) });
    report.steps.push(await observe("single search result"));
    // Let the single track end on its own, so the page moves into its autoplay suggestions.
    const duration = playback.at(-1)?.durationMs;
    if (duration && duration > 20_000) {
      report.steps.push({ seekNearEnd: await command("seek", { positionMs: duration - 6_000 }, null, 10_000) });
      const changed = await waitFor(() => playback.at(-1)?.currentTrack?.videoId && playback.at(-1).currentTrack.videoId !== seed, 30_000);
      report.steps.push({ advancedOnItsOwn: changed });
      report.steps.push(await observe("after the single track ended into autoplay"));
    }
    if (process.env.YTMCP_LIVE_FUTURE_RADIO === "1") {
      report.steps.push({ play: await command("playTarget", { target: { videoId: seed, playlistId: `RDAMVM${seed}` } }, (p) => p?.currentTrack?.videoId === seed) });
      report.steps.push(await observe("public radio of the same track"));
    }
    report.steps.push({ pause: await command("pause", null, (p) => p?.playback === "paused", 10_000) });
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
