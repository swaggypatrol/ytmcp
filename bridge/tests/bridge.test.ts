import { createHash } from "node:crypto";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  BridgeCommandAckPayload,
  BridgeHealthPayload,
  BridgePlaybackObservedPayload,
  BridgeReadyPayload,
  QueueSnapshot,
} from "@ytm-glass/protocol";
import { installBridge } from "../src/bridge.js";
import { installPageShim } from "../src/page-shim.js";
import { PAGE_SHIM_CHANNEL } from "../src/types.js";
import { createFakePage, createTransport, flush, settle } from "./fake-page.js";

const EPOCH = "bridge-test-epoch";
const handles: Array<{ uninstall(): void }> = [];

function track<T extends { uninstall(): void }>(handle: T): T {
  handles.push(handle);
  return handle;
}

function setup(options: Parameters<typeof createFakePage>[0] = {}) {
  const page = createFakePage(options);
  const t = createTransport();
  const handle = track(
    installBridge(t.transport, EPOCH, {
      windowRef: page.win,
      documentRef: page.doc,
      healthIntervalMs: 0,
      sampleIntervalMs: 1_000,
      mutationDebounceMs: 5,
    })
  );
  return { page, t, handle };
}

const ack = (t: ReturnType<typeof createTransport>, commandId: string) =>
  t.of<BridgeCommandAckPayload>("bridge.commandAck").find((a) => a.commandId === commandId);
const playback = (t: ReturnType<typeof createTransport>) => t.last<BridgePlaybackObservedPayload>("bridge.playbackObserved")!;
const queue = (t: ReturnType<typeof createTransport>) => t.last<QueueSnapshot>("bridge.queueObserved")!;

async function waitInitial(
  t: ReturnType<typeof createTransport>,
  requiredCaps: string[] = ["playPause", "seek"]
): Promise<void> {
  await t.waitForReady(requiredCaps);
  await t.waitFor((e) => e.kind === "bridge.playbackObserved", "initial bridge.playbackObserved");
}

async function dispatchCommand(
  t: ReturnType<typeof createTransport>,
  op: string,
  args: Record<string, unknown> | null = null,
  commandId?: string,
  timeoutMs?: number
) {
  const from = t.sent.length;
  const id = t.command(op, args, commandId);
  await t.waitForAck(id, from, timeoutMs);
  return { id, from };
}

/** Waits for a playbackObserved sent at or after `from` that satisfies `predicate`. */
function waitPlayback(
  t: ReturnType<typeof createTransport>,
  from: number,
  predicate: (p: BridgePlaybackObservedPayload) => boolean,
  label: string,
  timeoutMs?: number
) {
  return t.waitFor(
    (e) => e.kind === "bridge.playbackObserved" && predicate(e.payload as BridgePlaybackObservedPayload),
    label,
    from,
    timeoutMs
  );
}

/** Waits for a queueObserved sent at or after `from` that satisfies `predicate`. */
function waitQueue(
  t: ReturnType<typeof createTransport>,
  from: number,
  predicate: (q: QueueSnapshot) => boolean,
  label: string
) {
  return t.waitFor((e) => e.kind === "bridge.queueObserved" && predicate(e.payload as QueueSnapshot), label, from);
}

/**
 * Sends player.snapshotRequest and resolves once its forced refresh has emitted everything.
 * Only a forced refresh always emits bridge.health (setup disables the health interval), and
 * playbackObserved is the last envelope of a refresh.
 */
async function requestSnapshot(t: ReturnType<typeof createTransport>): Promise<void> {
  const from = t.sent.length;
  t.snapshotRequest();
  const health = await t.waitFor((e) => e.kind === "bridge.health", "forced bridge.health", from);
  await t.waitFor((e) => e.kind === "bridge.playbackObserved", "forced bridge.playbackObserved", t.sent.indexOf(health));
}

/**
 * Dispatches a command the page accepts and waits for its ack and for the follow-up refresh
 * the bridge runs after a dispatched command, so state the command left unchanged can be
 * asserted. Wait on a value predicate instead where the command should change state.
 */
async function dispatchObserved(
  t: ReturnType<typeof createTransport>,
  op: string,
  args: Record<string, unknown> | null = null,
  commandId?: string
) {
  const sent = await dispatchCommand(t, op, args, commandId);
  const ackEnv = await t.waitForAck(sent.id, sent.from);
  await t.waitFor((e) => e.kind === "bridge.playbackObserved", `follow-up after ${sent.id}`, t.sent.indexOf(ackEnv));
  return sent;
}

afterEach(() => {
  for (const h of handles.splice(0)) h.uninstall();
  vi.useRealTimers();
});

describe("bridge time model", () => {
  it("reports per-track position and duration from the player API, not the shared MSE media timeline", async () => {
    const { page, t } = setup();
    await waitInitial(t);
    // Third gapless track: the media element timeline already spans the previous tracks.
    Object.assign(page.media, { currentTime: 302.4, duration: 395.2, paused: false });
    Object.assign(page.player, { state: 1, timeSec: 2.4, lengthSec: 187 });
    const from = t.sent.length;
    page.fire("playing");
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).playback === "playing",
      "playing observation",
      from
    );

    const p = playback(t);
    expect(p.playback).toBe("playing");
    expect(p.positionMs).toBe(2400);
    expect(p.durationMs).toBe(187_000);
    expect(p.currentTrack?.durationMs).toBe(187_000);
  });

  it("seeks in the player API's per-track seconds and never writes media.currentTime", async () => {
    const { page, t } = setup();
    Object.assign(page.media, { currentTime: 302.4 });
    await waitInitial(t);
    const { id, from } = await dispatchCommand(t, "seek", { positionMs: 30_000 });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).positionMs === 30_000,
      "seeked observation",
      from
    );

    expect(ack(t, id)).toMatchObject({ dispatched: true, error: null });
    expect(page.api.seekTo).toHaveBeenCalledWith(30, true);
    expect(page.media.currentTime).toBe(302.4);
    expect(playback(t)).toMatchObject({ playback: "paused", positionMs: 30_000 });
  });

  it("observes the actual paused position even when the page accepts but does not apply a seek", async () => {
    const { page, t } = setup();
    page.api.seekTo.mockImplementationOnce(() => {});
    page.player.timeSec = 5;
    await waitInitial(t);
    const before = t.of("bridge.playbackObserved").length;
    const { id, from } = await dispatchCommand(t, "seek", { positionMs: 30_000 });
    await t.waitFor((e) => e.kind === "bridge.playbackObserved", "post-seek observation", from);

    expect(ack(t, id)).toMatchObject({ dispatched: true, error: null });
    expect(t.of("bridge.playbackObserved").length).toBeGreaterThan(before);
    expect(playback(t)).toMatchObject({ playback: "paused", positionMs: 5_000 });
  });

  it("keeps the instance when previous restarts the track, so the core sees the position jump back", async () => {
    const { page, t } = setup();
    await waitInitial(t);
    Object.assign(page.player, { state: 1, timeSec: 20 });
    const playingFrom = t.sent.length;
    page.fire("playing");
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).positionMs === 20_000,
      "playing position 20s",
      playingFrom
    );
    const before = playback(t);
    expect(before.positionMs).toBe(20_000);

    const prev = await dispatchCommand(t, "previous");
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).positionMs === 0,
      "restarted observation",
      prev.from
    );
    const restarted = playback(t);
    expect(restarted.positionMs).toBe(0);
    expect(restarted.trackInstanceId).toBe(before.trackInstanceId);
    expect(restarted.currentTrack?.videoId).toBe(before.currentTrack?.videoId);

    const next = await dispatchCommand(t, "next");
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).currentTrack?.videoId !== before.currentTrack?.videoId,
      "next track observation",
      next.from
    );
    const advanced = playback(t);
    expect(advanced.currentTrack?.videoId).not.toBe(before.currentTrack?.videoId);
    expect(advanced.trackInstanceId).not.toBe(before.trackInstanceId);
  });

  it("starts a new instance for a new load of the same video (duplicate queue occurrence)", async () => {
    const { page, t } = setup({ songs: [{ videoId: "dupsong0001" }, { videoId: "dupsong0001" }] });
    await waitInitial(t);
    Object.assign(page.player, { state: 1, timeSec: 179 });
    const playingFrom = t.sent.length;
    page.fire("playing");
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).playback === "playing",
      "first duplicate playing",
      playingFrom
    );
    const first = playback(t);

    const loadFrom = t.sent.length;
    page.loadIndex(1);
    page.fire("durationchange");
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).currentQueueEntryId !== first.currentQueueEntryId,
      "second duplicate load",
      loadFrom
    );
    const second = playback(t);
    expect(second.currentTrack?.videoId).toBe(first.currentTrack?.videoId);
    expect(second.currentQueueEntryId).not.toBe(first.currentQueueEntryId);
    expect(second.trackInstanceId).not.toBe(first.trackInstanceId);
  });
});

describe("bridge resource cadence", () => {
  it("samples timeupdate at most once per second and is event-driven while paused", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "setInterval", "Date"] });
    const page = createFakePage();
    const t = createTransport();
    let probes = 0;
    page.win.addEventListener("message", (e) => {
      const d = (e as MessageEvent).data as { direction?: string; op?: string };
      if (d?.direction === "to-page" && d.op === "probe") probes += 1;
    });
    track(installBridge(t.transport, EPOCH, { windowRef: page.win, documentRef: page.doc, healthIntervalMs: 12_000, sampleIntervalMs: 1_000 }));
    // The first probe hashes the account id with the real crypto.subtle, outside fake time.
    await waitInitial(t);
    await vi.advanceTimersByTimeAsync(10);
    const afterStart = probes;

    Object.assign(page.player, { state: 1 });
    for (let i = 0; i < 40; i++) {
      page.player.timeSec += 0.25;
      page.fire("timeupdate");
      await vi.advanceTimersByTimeAsync(250);
    }
    // 10 s of 4 Hz timeupdate -> about 10 samples, never 40.
    expect(probes - afterStart).toBeLessThanOrEqual(11);
    expect(probes - afterStart).toBeGreaterThanOrEqual(9);

    page.player.state = 2;
    page.fire("pause");
    await vi.advanceTimersByTimeAsync(10);
    const paused = probes;
    await vi.advanceTimersByTimeAsync(60_000);
    // Paused and idle: only the 12 s health heartbeat.
    expect(probes - paused).toBe(5);
    expect(t.of<BridgeHealthPayload>("bridge.health").length).toBeGreaterThanOrEqual(5);
  });
});

describe("bridge account hint", () => {
  const expected = (id: string) =>
    `acct:${createHash("sha256").update(`ytm-glass-account-v1\n${id}`, "utf8").digest("hex").slice(0, 32)}`;

  it("sends the hashed DATASYNC_ID hint, re-sends bridge.ready when it changes, and never leaks the raw id", async () => {
    const { page, t } = setup({ datasyncId: "synthAccountAAAA00001||" });
    await waitInitial(t);
    expect(t.last<BridgeReadyPayload>("bridge.ready")).toMatchObject({
      loggedInObserved: true,
      accountHint: expected("synthAccountAAAA00001||"),
    });

    const readyCount = t.of("bridge.ready").length;
    page.cfg.DATASYNC_ID = "synthAccountBBBB00002||";
    const changedFrom = t.sent.length;
    page.fire("play");
    // The refresh that hashes the new id ends with its playbackObserved, after its one ready.
    const changedReady = await t.waitFor((e) => e.kind === "bridge.ready", "re-sent bridge.ready", changedFrom);
    await t.waitFor((e) => e.kind === "bridge.playbackObserved", "account refresh end", t.sent.indexOf(changedReady));
    expect(t.of("bridge.ready").length).toBe(readyCount + 1);
    expect(t.last<BridgeReadyPayload>("bridge.ready")?.accountHint).toBe(expected("synthAccountBBBB00002||"));

    page.cfg.LOGGED_IN = false;
    const logoutFrom = t.sent.length;
    page.fire("play");
    await t.waitFor((e) => e.kind === "bridge.ready", "logged-out bridge.ready", logoutFrom);
    expect(t.last<BridgeReadyPayload>("bridge.ready")).toMatchObject({ loggedInObserved: false, accountHint: null });

    expect(JSON.stringify(t.sent)).not.toContain("synthAccount");
  });
});

describe("bridge capability updates", () => {
  it("re-sends bridge.ready once a capability change holds, and not for a control that blinks out", async () => {
    const page = createFakePage();
    const t = createTransport();
    track(
      installBridge(t.transport, EPOCH, {
        windowRef: page.win,
        documentRef: page.doc,
        healthIntervalMs: 0,
        mutationDebounceMs: 5,
        readyDebounceMs: 60,
      })
    );
    // The first probe hashes the account id with the real crypto.subtle; the debounce
    // windows below then run on fake time so a stalled event loop cannot reorder them.
    await waitInitial(t, ["shuffle"]);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.shuffle).toBe("supported");
    const readyCount = t.of("bridge.ready").length;
    const bar = page.doc.querySelector("ytmusic-player-bar")!;
    const shuffle = bar.querySelector(".shuffle")!;

    // Removed and restored within the debounce window (e.g. a player bar re-render).
    shuffle.remove();
    await vi.advanceTimersByTimeAsync(20);
    expect(t.of("bridge.ready").length).toBe(readyCount);
    bar.append(shuffle);
    await vi.advanceTimersByTimeAsync(120);
    expect(t.of("bridge.ready").length).toBe(readyCount);

    // A lasting change is reported once, after the debounce.
    shuffle.remove();
    await vi.advanceTimersByTimeAsync(20);
    expect(t.of("bridge.ready").length).toBe(readyCount);
    await vi.advanceTimersByTimeAsync(120);
    expect(t.of("bridge.ready").length).toBe(readyCount + 1);
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.shuffle).toBe("unsupported");
    const { id } = await dispatchCommand(t, "setShuffle", { shuffle: true });
    expect(ack(t, id)).toMatchObject({ dispatched: false, error: { code: "UNSUPPORTED_CAPABILITY" } });

    bar.append(shuffle);
    await vi.advanceTimersByTimeAsync(120);
    expect(t.of("bridge.ready").length).toBe(readyCount + 2);
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.shuffle).toBe("supported");
  });

  it("restarts the wait when the capabilities change again before the debounce ends", async () => {
    const page = createFakePage();
    const t = createTransport();
    track(
      installBridge(t.transport, EPOCH, {
        windowRef: page.win,
        documentRef: page.doc,
        healthIntervalMs: 0,
        mutationDebounceMs: 5,
        readyDebounceMs: 150,
      })
    );
    await waitInitial(t, ["shuffle", "repeat"]);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const readyCount = t.of("bridge.ready").length;
    const bar = page.doc.querySelector("ytmusic-player-bar")!;

    bar.querySelector(".shuffle")!.remove();
    await vi.advanceTimersByTimeAsync(90);
    bar.querySelector(".repeat")!.remove();
    // 180 ms after the first change, but the second one has held for less than 150 ms.
    await vi.advanceTimersByTimeAsync(90);
    expect(t.of("bridge.ready").length).toBe(readyCount);
    await vi.advanceTimersByTimeAsync(200);
    expect(t.of("bridge.ready").length).toBe(readyCount + 1);
    const caps = t.last<BridgeReadyPayload>("bridge.ready")!.capabilities;
    expect(caps.shuffle).toBe("unsupported");
    expect(caps.repeat).toBe("unsupported");
  });
});

describe("bridge queue mirror", () => {
  it("recovers enqueue when the page defines its handler after the first snapshot", async () => {
    const { page, t } = setup({ deferQueueAddHandler: true });
    await waitInitial(t, ["queueMirror"]);
    expect(t.last<BridgeReadyPayload>("bridge.ready")).toMatchObject({
      bridgeEpoch: EPOCH,
      capabilities: { queueMirror: "supported", queueEnqueue: "unsupported" },
    });

    page.registerQueueAddHandler();
    const refreshFrom = t.sent.length;
    t.snapshotRequest();
    await t.waitForReady(["queueEnqueue"], refreshFrom);
    expect(t.last<BridgeReadyPayload>("bridge.ready")).toMatchObject({
      bridgeEpoch: EPOCH,
      capabilities: { queueEnqueue: "supported" },
    });
    const enq = await dispatchCommand(t, "enqueue", { target: { videoId: "lateHandlerSong" }, enqueuePosition: "next" });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.queueObserved" &&
        ((e.payload as QueueSnapshot).entries?.some((entry) => entry.videoId === "lateHandlerSong") ?? false),
      "post-enqueue queueObserved",
      enq.from
    );
    expect(ack(t, enq.id)).toMatchObject({ dispatched: true });
    expect(queue(t).entries?.map((entry) => entry.videoId)).toEqual(["song00000a1", "lateHandlerSong", "song00000b2"]);
  });

  it("waits before retrying a registered but not yet usable queue handler", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const { page, t } = setup({ datasyncId: null });
    const create = page.doc.createElement.bind(page.doc);
    let attempts = 0;
    vi.spyOn(page.doc, "createElement").mockImplementation((tag, options) => {
      if (tag === "ytmusic-menu-service-item-renderer" && ++attempts === 1) return create("div");
      return create(tag, options);
    });
    const firstSeenAt = Date.now();
    await waitInitial(t, ["queueMirror"]);
    expect(t.last<BridgeReadyPayload>("bridge.ready")?.capabilities.queueEnqueue).toBe("unsupported");
    await requestSnapshot(t);
    expect(attempts).toBe(1);

    vi.setSystemTime(firstSeenAt + 500);
    await requestSnapshot(t);
    expect(attempts).toBe(2);
    expect(t.last<BridgeReadyPayload>("bridge.ready")?.capabilities.queueEnqueue).toBe("supported");
  });

  it("claims no current occurrence while the queue selection lags behind the playing video", async () => {
    const { page, t } = setup({ queueEdits: true, songs: [{ videoId: "selectedRow" }, { videoId: "playingSong" }] });
    await waitInitial(t, ["queueMirror"]);
    expect(playback(t).currentQueueEntryId).toBe(queue(t).currentEntryId);
    expect(queue(t).currentEntryId).not.toBeNull();

    // The player moves on while the official queue keeps selecting the first row.
    page.player.videoId = "playingSong";
    page.player.cpnCounter += 1;
    const lagFrom = t.sent.length;
    t.snapshotRequest();
    await t.waitFor((e) => e.kind === "bridge.playbackObserved", "lagged playbackObserved", lagFrom);
    expect(queue(t).currentEntryId).toBeNull();
    expect(playback(t)).toMatchObject({ currentQueueEntryId: null, currentTrack: { videoId: "playingSong" } });

    const alignFrom = t.sent.length;
    page.loadIndex(1);
    t.snapshotRequest();
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).currentQueueEntryId !== null,
      "aligned playbackObserved",
      alignFrom
    );
    expect(queue(t).currentEntryId).toBe(queue(t).entries![1]!.entryId);
    expect(playback(t).currentQueueEntryId).toBe(queue(t).currentEntryId);
  });

  it("refuses a queue row that displays another occurrence of the same video", async () => {
    const { page, t } = setup({ songs: [
      { videoId: "currentSong" },
      { videoId: "duplicateSong" },
      { videoId: "duplicateSong" },
    ] });
    await waitInitial(t, ["queueMirror"]);
    const observed = queue(t);
    const contents = page.doc.querySelector("ytmusic-player-queue #contents")!;
    contents.insertBefore(contents.children[2]!, contents.children[1]!);
    const { id } = await dispatchCommand(t, "selectQueueEntry", {
      queueEntryId: observed.entries![1]!.entryId,
      queueRevision: observed.queueRevision,
    });
    expect(ack(t, id)).toMatchObject({ dispatched: false, error: { code: "STALE_CONTEXT" } });
    await settle(); // Nothing else happens: no late selection after the refusal.
    expect(page.queue.selectedItemIndex).toBe(0);
  });

  it("keeps an oversized or autoplay queue partial and rejects selection outside the rendered DOM", async () => {
    const songs = Array.from({ length: 230 }, (_, i) => ({ videoId: `boundedSong${i}` }));
    const { page, t } = setup({ songs });
    page.loadIndex(220);
    await waitInitial(t, ["queueMirror"]);
    const observed = queue(t);
    expect(observed).toMatchObject({ loadedRange: { start: 30, endExclusive: 230 }, completeness: "partial" });
    expect(observed.entries).toHaveLength(200);
    expect(observed.entries![190]).toMatchObject({ entryId: observed.currentEntryId, videoId: "boundedSong220" });

    page.doc.querySelector("ytmusic-player-queue #contents")!.replaceChildren();
    const { id } = await dispatchCommand(t, "selectQueueEntry", {
      queueEntryId: observed.entries![191]!.entryId,
      queueRevision: observed.queueRevision,
    });
    expect(ack(t, id)).toMatchObject({ dispatched: false, error: { code: "STALE_CONTEXT" } });
    await settle(); // Nothing else happens: no late selection after the refusal.
    expect(page.queue.selectedItemIndex).toBe(220);

    page.queue.items.splice(2);
    page.loadIndex(0);
    page.queue.automixItems.push({ videoId: "unobservedAutoplayTail" });
    await requestSnapshot(t);
    expect(queue(t)).toMatchObject({ completeness: "partial", hasMore: true });
    expect(queue(t).entries?.map((entry) => entry.videoId)).toEqual(["boundedSong0", "boundedSong1"]);
  });

  it("keeps occurrence ids across an insert, distinguishes duplicate videoIds, and selects the exact occurrence", async () => {
    const { page, t } = setup({
      songs: [{ videoId: "songAAAAAAA" }, { videoId: "songBBBBBBB" }, { videoId: "songAAAAAAA" }],
    });
    await waitInitial(t, ["queueMirror", "queueEnqueue", "playPause"]);
    const q1 = queue(t);
    expect(q1.entries?.map((e) => e.videoId)).toEqual(["songAAAAAAA", "songBBBBBBB", "songAAAAAAA"]);
    const ids1 = q1.entries!.map((e) => e.entryId);
    expect(new Set(ids1).size).toBe(3);
    expect(ids1.every((id) => id.startsWith(`qe:${EPOCH}:`))).toBe(true);
    expect(q1.currentEntryId).toBe(ids1[0]);
    expect(q1.completeness).toBe("complete");

    const enq = await dispatchCommand(t, "enqueue", { target: { videoId: "songCCCCCCC" }, enqueuePosition: "next" });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.queueObserved" &&
        ((e.payload as QueueSnapshot).entries?.some((entry) => entry.videoId === "songCCCCCCC") ?? false),
      "enqueue queueObserved",
      enq.from
    );
    expect(ack(t, enq.id)).toMatchObject({ dispatched: true });
    const q2 = queue(t);
    expect(q2.queueRevision).toBeGreaterThan(q1.queueRevision);
    expect(q2.entries?.map((e) => e.videoId)).toEqual(["songAAAAAAA", "songCCCCCCC", "songBBBBBBB", "songAAAAAAA"]);
    const ids2 = q2.entries!.map((e) => e.entryId);
    expect([ids2[0], ids2[2], ids2[3]]).toEqual(ids1);
    expect(ids1).not.toContain(ids2[1]);

    // Select the second occurrence of the duplicate video, not the first.
    const sel = await dispatchCommand(t, "selectQueueEntry", { queueEntryId: ids2[3], queueRevision: q2.queueRevision });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).currentQueueEntryId === ids2[3],
      "selected duplicate playbackObserved",
      sel.from
    );
    expect(ack(t, sel.id)).toMatchObject({ dispatched: true });
    expect(page.queue.selectedItemIndex).toBe(3);
    expect(playback(t).currentQueueEntryId).toBe(ids2[3]);

    const { id: stale } = await dispatchCommand(t, "selectQueueEntry", {
      queueEntryId: ids2[1],
      queueRevision: q1.queueRevision,
    });
    expect(ack(t, stale)?.error?.code).toBe("STALE_CONTEXT");
  });

  it("gives ambiguous occurrences fresh ids instead of guessing", async () => {
    const { page, t } = setup({ songs: [{ videoId: "songAAAAAAA", psv: "same" }, { videoId: "songBBBBBBB" }] });
    await waitInitial(t, ["queueMirror"]);
    const before = queue(t).entries!.map((e) => e.entryId);
    // The page now reports two rows with an identical occurrence key.
    const clone = structuredClone(page.queue.items[0]!);
    page.queue.items.push(clone);
    page.renderQueue();
    const ambigFrom = t.sent.length;
    t.snapshotRequest();
    await t.waitFor((e) => e.kind === "bridge.queueObserved", "ambiguous queueObserved", ambigFrom);
    const after = queue(t).entries!.map((e) => e.entryId);
    expect(after[1]).toBe(before[1]);
    expect(after[0]).not.toBe(before[0]);
    expect(after[2]).not.toBe(before[0]);
    expect(new Set(after).size).toBe(3);
    const { id } = await dispatchCommand(t, "selectQueueEntry", {
      queueEntryId: after[2],
      queueRevision: queue(t).queueRevision,
    });
    expect(ack(t, id)).toMatchObject({ dispatched: false, error: { code: "STALE_CONTEXT" } });
    await settle(); // Nothing else happens: no late selection after the refusal.
    expect(page.queue.selectedItemIndex).toBe(0);
  });

  it("keeps ids and revision across refreshes of an unchanged queue with unparsable and ambiguous rows", async () => {
    const { page, t } = setup({ songs: [{ videoId: "songAAAAAAA", psv: "same" }, { videoId: "songAAAAAAA", psv: "same" }] });
    page.queue.items.push({} as (typeof page.queue.items)[number]);
    page.renderQueue();
    await waitInitial(t);
    await requestSnapshot(t);
    const q1 = queue(t);
    const revisions = t.of<QueueSnapshot>("bridge.queueObserved").length;
    await requestSnapshot(t);
    page.player.timeSec = 5;
    page.fire("timeupdate");
    // Nothing else happens: an unchanged queue emits no queueObserved on a later refresh.
    await settle();
    const q2 = queue(t);
    expect(q2.queueRevision).toBe(q1.queueRevision);
    expect(q2.entries!.map((e) => e.entryId)).toEqual(q1.entries!.map((e) => e.entryId));
    // The forced snapshot re-sends the same revision; nothing else is emitted.
    expect(t.of<QueueSnapshot>("bridge.queueObserved").length).toBe(revisions + 1);
  });

  it("enqueue last appends on a finite queue and is rejected explicitly on an endless radio queue", async () => {
    const { page, t } = setup();
    await waitInitial(t, ["queueMirror", "queueEnqueue"]);
    const { id: last } = await dispatchCommand(t, "enqueue", { target: { videoId: "songZZZZZZZ" }, enqueuePosition: "last" });
    expect(ack(t, last)).toMatchObject({ dispatched: true });
    expect(page.queueAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ queueAddEndpoint: expect.objectContaining({ queueInsertPosition: "INSERT_AT_END" }) })
    );

    page.queue.isInfinite = true;
    await requestSnapshot(t);
    expect(queue(t)).toMatchObject({ completeness: "partial", hasMore: true });
    const calls = page.queueAdd.mock.calls.length;
    const { id: radio } = await dispatchCommand(t, "enqueue", { target: { videoId: "songZZZZZZZ" }, enqueuePosition: "last" });
    expect(ack(t, radio)).toMatchObject({ dispatched: false, error: { code: "UNSUPPORTED_CAPABILITY" } });
    await settle(); // Nothing else happens: the refused append never reaches the page handler.
    expect(page.queueAdd.mock.calls.length).toBe(calls);
  });
});

describe("bridge queue edits", () => {
  const songs = [
    { videoId: "songAAAAAAA" }, { videoId: "songBBBBBBB", psv: "first" },
    { videoId: "songBBBBBBB", psv: "second" }, { videoId: "songCCCCCCC" },
  ];

  it("removes the exact duplicate occurrence without a rendered row or playback change", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    page.doc.querySelector("#contents")!.replaceChildren();
    await waitInitial(t, ["queueMirror", "queueRemove"]);
    const before = queue(t);
    const initialTrack = playback(t).trackInstanceId;
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.queueRemove).toBe("supported");
    const args = { queueEntryId: before.entries![2]!.entryId, queueRevision: before.queueRevision };
    const { id, from } = await dispatchCommand(t, "removeQueueEntry", args);
    await waitQueue(t, from, (q) => q.entries?.length === 3, "queue after removal");

    expect(page.queueDispatch).toHaveBeenCalledExactlyOnceWith({ type: "REMOVE_ITEM", payload: 2 });
    expect(ack(t, id)).toMatchObject({ dispatched: true, error: null });
    expect(queue(t).entries!.map((entry) => entry.entryId)).toEqual([0, 1, 3].map((index) => before.entries![index]!.entryId));
    expect(playback(t).trackInstanceId).toBe(initialTrack);
    await dispatchCommand(t, "removeQueueEntry", args, id);
    await settle(); // Nothing else happens: the retransmit is answered from the cache.
    expect(page.queueDispatch).toHaveBeenCalledTimes(1);
  });

  it("moves one duplicate ahead of an occurrence and to the known finite tail", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    await waitInitial(t, ["queueMirror", "queueReorder"]);
    const before = queue(t);
    const [current, first, target, last] = before.entries!.map((entry) => entry.entryId);
    const { id, from } = await dispatchCommand(t, "moveQueueEntry", {
      queueEntryId: target, beforeQueueEntryId: first, queueRevision: before.queueRevision,
    });
    await waitQueue(t, from, (q) => q.queueRevision > before.queueRevision, "queue after first move");
    expect(page.queueDispatch).toHaveBeenNthCalledWith(1, { type: "MOVE_ITEM", payload: { fromIndex: 2, toIndex: 1 } });
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    const moved = queue(t);
    expect(moved.entries!.map((entry) => entry.entryId)).toEqual([current, target, first, last]);

    const tail = await dispatchCommand(t, "moveQueueEntry", { queueEntryId: target, queueRevision: moved.queueRevision });
    await waitQueue(t, tail.from, (q) => q.queueRevision > moved.queueRevision, "queue after move to the tail");
    expect(page.queueDispatch).toHaveBeenNthCalledWith(2, { type: "MOVE_ITEM", payload: { fromIndex: 1, toIndex: 3 } });
    expect(queue(t).entries!.map((entry) => entry.entryId)).toEqual([current, first, last, target]);
    expect(queue(t).currentEntryId).toBe(current);
  });

  it("rejects a stale order or playback load even when the old bridge revision is still visible", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    await waitInitial(t, ["queueMirror", "queueRemove"]);
    const before = queue(t);
    const args = { queueEntryId: before.entries![2]!.entryId, queueRevision: before.queueRevision };
    const first = page.queue.items[1]!;
    const last = page.queue.items[3]!;
    page.queue.items[1] = last;
    page.queue.items[3] = first;
    const { id: staleOrder } = await dispatchCommand(t, "removeQueueEntry", args);
    expect(ack(t, staleOrder)?.error?.code).toBe("STALE_CONTEXT");

    page.queue.items[1] = first;
    page.queue.items[3] = last;
    page.player.cpnCounter += 1;
    const { id: staleLoad } = await dispatchCommand(t, "removeQueueEntry", args);
    expect(ack(t, staleLoad)?.error?.code).toBe("STALE_CONTEXT");
    await settle(); // Nothing else happens: neither refusal reaches the store.
    expect(page.queueDispatch).not.toHaveBeenCalled();
  });

  it("requires the active store, a finite exact queue and a different occurrence from current playback", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    await waitInitial(t, ["queueMirror", "queueRemove", "queueReorder"]);
    const before = queue(t);
    const args = { queueEntryId: before.currentEntryId, queueRevision: before.queueRevision };
    const { id: current } = await dispatchCommand(t, "removeQueueEntry", args);
    expect(ack(t, current)?.error?.code).toBe("INVALID_PAYLOAD");

    page.queueStore.getState = () => ({ queue: { ...page.queue } });
    const { id: separateStore } = await dispatchCommand(t, "removeQueueEntry", { ...args, queueEntryId: before.entries![1]!.entryId });
    expect(ack(t, separateStore)?.error?.code).toBe("UNSUPPORTED_CAPABILITY");

    page.queueStore.getState = () => ({ queue: page.queue });
    page.queue.automixItems = [{}];
    const { id: partial } = await dispatchCommand(t, "moveQueueEntry", { ...args, queueEntryId: before.entries![1]!.entryId });
    expect(ack(t, partial)?.error?.code).toBe("UNSUPPORTED_CAPABILITY");
    await settle(); // Nothing else happens: no refused edit reaches the store.
    expect(page.queueDispatch).not.toHaveBeenCalled();
  });

  it("reports dispatch without fabricating an effect when the page ignores an edit", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    page.queueDispatch.mockImplementationOnce(() => { throw new Error("store subscriber"); });
    await waitInitial(t, ["queueMirror", "queueRemove"]);
    const before = queue(t);
    const { id } = await dispatchObserved(t, "removeQueueEntry", { queueEntryId: before.entries![1]!.entryId, queueRevision: before.queueRevision });
    await settle(); // Nothing else happens: the follow-up refresh read back an unchanged queue.
    expect(ack(t, id)).toMatchObject({ dispatched: true, error: null });
    expect(queue(t)).toEqual(before);
  });
});

describe("bridge clear upcoming", () => {
  const songs = [
    { videoId: "songAAAAAAA", psv: "past" }, { videoId: "songBBBBBBB", psv: "current" },
    { videoId: "songBBBBBBB", psv: "repeat" }, { videoId: "songAAAAAAA", psv: "tail" },
  ];
  const args = (t: ReturnType<typeof createTransport>) => ({
    queueEntryId: queue(t).currentEntryId, queueRevision: queue(t).queueRevision,
    trackInstanceId: playback(t).trackInstanceId,
  });

  it("removes only the upcoming occurrences while retaining duplicate songs in the past and present", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    page.loadIndex(1);
    Object.assign(page.player, { state: 1, timeSec: 15 });
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    const playbackBefore = playback(t).trackInstanceId;
    const commandArgs = args(t);
    const { id: clear, from } = await dispatchCommand(t, "clearUpcoming", commandArgs);
    await waitQueue(t, from, (q) => q.entries?.length === 2, "queue after clearing");
    expect(ack(t, clear)).toMatchObject({ dispatched: true });
    expect(page.queueDispatch.mock.calls).toEqual([3, 2].map((index) => [{ type: "REMOVE_ITEM", payload: index }]));
    expect(queue(t).entries).toEqual(before.entries!.slice(0, 2));
    expect(queue(t).currentEntryId).toBe(before.currentEntryId);
    expect(playback(t)).toMatchObject({ trackInstanceId: playbackBefore, playback: "playing", positionMs: 15000 });

    // A retransmit and a fresh clear of the already-empty tail, sent back to back.
    const againFrom = t.sent.length;
    t.command("clearUpcoming", commandArgs, clear);
    const alreadyCleared = t.command("clearUpcoming", args(t));
    await t.waitForAck(clear, againFrom);
    await t.waitForAck(alreadyCleared, againFrom);
    expect(ack(t, alreadyCleared)).toMatchObject({ dispatched: true });
    await settle(); // Nothing else happens: neither command removes anything more.
    expect(page.queueDispatch).toHaveBeenCalledTimes(2);
  });

  it("stops if the store replaces any surviving occurrence after a removal", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    page.loadIndex(1);
    const dispatch = page.queueDispatch.getMockImplementation()!;
    page.queueDispatch.mockImplementationOnce((action) => {
      dispatch(action);
      page.queue.items[2]!.playlistPanelVideoRenderer.playlistSetVideoId = "external-replacement";
    });
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    const { id: clear, from } = await dispatchCommand(t, "clearUpcoming", args(t));
    await waitQueue(t, from, (q) => q.entries?.length === 3, "queue after the interrupted clear");
    await settle(); // Nothing else happens: the clear stops after the replaced occurrence.
    expect(ack(t, clear)).toMatchObject({ dispatched: true });
    expect(page.queueDispatch).toHaveBeenCalledExactlyOnceWith({ type: "REMOVE_ITEM", payload: 3 });
    expect(queue(t).entries!.slice(0, 2)).toEqual(before.entries!.slice(0, 2));
    expect(queue(t).entries).toHaveLength(3);
  });

  it("stops after the first native call if the page did not apply the removal", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    page.queueDispatch.mockImplementationOnce(() => {});
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    const { id: clear } = await dispatchObserved(t, "clearUpcoming", args(t));
    await settle(); // Nothing else happens: no further removal follows the ignored one.
    expect(ack(t, clear)).toMatchObject({ dispatched: true });
    expect(page.queueDispatch).toHaveBeenCalledExactlyOnceWith({ type: "REMOVE_ITEM", payload: 3 });
    expect(queue(t)).toEqual(before);
  });

  it("does not schedule another removal after a reducer applied one and its subscriber threw", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    const dispatch = page.queueDispatch.getMockImplementation()!;
    page.queueDispatch.mockImplementationOnce((action) => {
      dispatch(action);
      throw new Error("subscriber failed after dispatch");
    });
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    const { id: clear, from } = await dispatchCommand(t, "clearUpcoming", args(t));
    await waitQueue(t, from, (q) => q.entries?.length === 3, "queue after the first removal");
    await settle(); // Nothing else happens: no removal is scheduled after the throw.
    expect(ack(t, clear)).toMatchObject({ dispatched: true });
    expect(page.queueDispatch).toHaveBeenCalledExactlyOnceWith({ type: "REMOVE_ITEM", payload: 3 });
    expect(queue(t).entries).toEqual(before.entries!.slice(0, 3));
  });

  it("removes only the nearest 50 upcoming entries of a longer queue", async () => {
    const { page, t } = setup({ queueEdits: true, songs: Array.from({ length: 53 }, (_, i) => ({ videoId: `synthTrack${i}` })) });
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    const { id: clear, from } = await dispatchCommand(t, "clearUpcoming", args(t));
    await waitQueue(t, from, (q) => q.entries?.length === 3, "queue after clearing 50");
    await settle(); // Nothing else happens: the entries past the nearest 50 stay.
    expect(ack(t, clear)).toMatchObject({ dispatched: true, error: null });
    expect(page.queueDispatch.mock.calls).toEqual(
      Array.from({ length: 50 }, (_, i) => [{ type: "REMOVE_ITEM", payload: 50 - i }]),
    );
    expect(queue(t).entries).toEqual([before.entries![0], before.entries![51], before.entries![52]]);
  });

  it("rejects a stale current target and a radio queue before dispatch", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const { id: stale } = await dispatchCommand(t, "clearUpcoming", { ...args(t), queueEntryId: queue(t).entries![1]!.entryId });
    expect(ack(t, stale)?.error?.code).toBe("STALE_CONTEXT");

    page.queue.isInfinite = true;
    const { id: radio } = await dispatchCommand(t, "clearUpcoming", args(t));
    expect(ack(t, radio)?.error?.code).toBe("UNSUPPORTED_CAPABILITY");
    await settle(); // Nothing else happens: neither refusal reaches the store.
    expect(page.queueDispatch).not.toHaveBeenCalled();
  });

  it("removes every loaded upcoming entry despite a continuation and reports that more may load", async () => {
    const { page, t } = setup({ queueEdits: true, songs });
    page.queueNative.continuation = "synth-continuation";
    page.loadIndex(1);
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    expect(before).toMatchObject({ completeness: "complete", hasMore: false, mayLoadMore: true });
    const { id: clear, from } = await dispatchCommand(t, "clearUpcoming", args(t));
    await waitQueue(t, from, (q) => q.entries?.length === 2, "queue after clearing");
    await settle(); // Nothing else happens: the continuation does not extend the clear.
    expect(ack(t, clear)).toMatchObject({ dispatched: true, error: null });
    expect(page.queueDispatch.mock.calls).toEqual([3, 2].map((index) => [{ type: "REMOVE_ITEM", payload: index }]));
    expect(queue(t)).toMatchObject({ currentEntryId: before.currentEntryId, mayLoadMore: true });
    expect(queue(t).entries).toEqual(before.entries!.slice(0, 2));
  });

  it("keeps autoplay suggestions, which are not queue entries, and leaves the explicit end provable", async () => {
    const { page, t } = setup({ queueEdits: true, autoplay: true, songs });
    page.queue.automixItems.push({ videoId: "synthTail000" });
    page.loadIndex(1);
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const before = queue(t);
    const { id: clear, from } = await dispatchCommand(t, "clearUpcoming", args(t));
    await waitQueue(t, from, (q) => q.entries?.length === 2, "queue after clearing");
    await settle(); // Nothing else happens: the autoplay suggestions stay.
    expect(ack(t, clear)).toMatchObject({ dispatched: true, error: null });
    expect(page.queue.automixItems).toEqual([{ videoId: "synthTail000" }]);
    expect(queue(t)).toMatchObject({ hasMore: true, explicitComplete: true, mayLoadMore: false });
    expect(queue(t).entries).toEqual(before.entries!.slice(0, 2));
  });

  it("addresses absolute positions when the reported window starts past the first entry", async () => {
    const { page, t } = setup({ queueEdits: true, songs: Array.from({ length: 230 }, (_, i) => ({ videoId: `boundedSong${i}` })) });
    page.loadIndex(225);
    await waitInitial(t, ["queueMirror", "queueClear"]);
    expect(queue(t).loadedRange).toEqual({ start: 30, endExclusive: 230 });
    const { id: clear } = await dispatchCommand(t, "clearUpcoming", args(t));
    await settle(); // Nothing else happens: only the four upcoming positions are removed.
    expect(ack(t, clear)).toMatchObject({ dispatched: true, error: null });
    expect(page.queueDispatch.mock.calls).toEqual([229, 228, 227, 226].map((index) => [{ type: "REMOVE_ITEM", payload: index }]));
    expect(page.queue.items).toHaveLength(226);
    expect(page.queue.selectedItemIndex).toBe(225);
  });
});

describe("bridge playback diagnostics", () => {
  it("projects only the current audio codec, player label and explicit Song/Video selection", async () => {
    const { page, t } = setup();
    Object.assign(page.player, { state: 1, timeSec: 2 });
    page.api.getStatsForNerds.mockReturnValue({ codecs: "0 / mp4a.40.2 (141)", unrelated: "not-projected" });
    page.api.getPlaybackQuality.mockReturnValue("auto");
    const toggle = page.doc.createElement("ytmusic-av-toggle");
    toggle.setAttribute("selected-queue-item-has-counterpart", "");
    toggle.setAttribute("is-video-playback-mode-selected", "false");
    page.doc.body.append(toggle);
    await waitInitial(t);
    expect(playback(t)).toMatchObject({ codec: "mp4a.40.2", qualityLabel: "auto", audioMode: "song", drmProtected: null });
    expect(JSON.stringify(playback(t))).not.toContain("not-projected");

    page.api.getStatsForNerds.mockReturnValue({ codecs: "vp09.00.51.08 (248) / opus (251)" });
    page.api.getPlaybackQuality.mockReturnValue("hd720");
    toggle.setAttribute("is-video-playback-mode-selected", "true");
    await requestSnapshot(t);
    expect(playback(t)).toMatchObject({ codec: "opus", qualityLabel: "hd720", audioMode: "video" });

    page.loadIndex(1);
    await requestSnapshot(t);
    expect(playback(t)).toMatchObject({ codec: null, qualityLabel: null, audioMode: null });
  });

  it("withholds diagnostics for malformed values and an ad playback window", async () => {
    const { page, t } = setup();
    Object.assign(page.player, { state: 1, timeSec: 2 });
    page.api.getStatsForNerds.mockReturnValue({ codecs: "vp9 (248) / 251", afmt: "251" });
    page.api.getPlaybackQuality.mockReturnValue("https://example.invalid/not-a-quality-label");
    const toggle = page.doc.createElement("ytmusic-av-toggle");
    toggle.setAttribute("selected-queue-item-has-counterpart", "");
    page.doc.body.append(toggle);
    await waitInitial(t);
    expect(playback(t)).toMatchObject({ codec: null, qualityLabel: null, audioMode: null, drmProtected: null });

    page.api.getStatsForNerds.mockReturnValue({ codecs: "0 / opus (251)" });
    page.api.getPlaybackQuality.mockReturnValue("auto");
    page.doc.getElementById("movie_player")!.classList.add("ad-showing");
    await requestSnapshot(t);
    expect(playback(t)).toMatchObject({ codec: null, qualityLabel: null, audioMode: null });
  });
});

describe("bridge queue clear capability", () => {
  it("keeps clearing supported with a continuation or an autoplay tail but not on a radio queue", async () => {
    const { page, t } = setup({ queueEdits: true, autoplay: true });
    page.queueNative.continuation = "synthetic-continuation";
    page.queue.automixItems.push({ videoId: "synthTail000" });
    await waitInitial(t, ["queueMirror", "queueClear"]);
    const caps = t.last<BridgeReadyPayload>("bridge.ready")!.capabilities;
    expect(caps.queueClear).toBe("supported");
    // Single edits still need a finite exact queue.
    expect(caps.queueRemove).toBe("unsupported");
    page.queue.isInfinite = true;
    const radioFrom = t.sent.length;
    t.snapshotRequest();
    await t.waitFor(
      (e) => e.kind === "bridge.ready" && (e.payload as BridgeReadyPayload).capabilities.queueClear === "unsupported",
      "bridge.ready without queueClear",
      radioFrom
    );
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.queueClear).toBe("unsupported");
  });
});

describe("bridge results after page clicks", () => {
  it("reports clicks that missed their target as dispatched so the core observes the outcome", async () => {
    const { page, t } = setup();
    await waitInitial(t, ["playPause", "shuffle"]);
    // The page ignores the shuffle click: the shim reports NO_EFFECT after clicking.
    page.doc.querySelector(".shuffle")!.addEventListener("click", (event) => event.stopImmediatePropagation(), { capture: true });
    const { id } = await dispatchCommand(t, "setShuffle", { shuffle: true });
    expect(ack(t, id)).toMatchObject({ dispatched: true, error: null });
  });
});

describe("bridge queue autoplay", () => {
  it("finds the switch in the side-panel tab header and in the legacy queue placement", async () => {
    for (const legacyAutoplaySwitch of [false, true]) {
      const { page, t } = setup({ autoplay: true, legacyAutoplaySwitch });
      await waitInitial(t, ["queueAutoplay"]);
      expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.queueAutoplay).toBe("supported");
      const { id: off, from } = await dispatchCommand(t, "setAutoplay", { autoplay: false });
      await waitPlayback(t, from, (p) => p.autoplay === false, "autoplay=false");
      expect(ack(t, off)).toMatchObject({ dispatched: true });
      expect(page.toggleAutoplay).toHaveBeenCalledTimes(1);
      expect(playback(t).autoplay).toBe(false);
    }
  });

  it("treats kept autoplay suggestions as no tail once autoplay is off", async () => {
    const { page, t } = setup({ autoplay: true, queueEdits: true });
    page.queue.automixItems.push({ videoId: "synthTail000" });
    await waitInitial(t, ["queueMirror"]);
    expect(t.last<QueueSnapshot>("bridge.queueObserved")).toMatchObject({ completeness: "partial", hasMore: true });
    page.queue.autoplay = false;
    await requestSnapshot(t);
    expect(t.last<QueueSnapshot>("bridge.queueObserved")).toMatchObject({
      completeness: "complete", hasMore: false, explicitComplete: true,
    });
  });

  it("reads the queue setting and clicks the page toggle only when its target differs", async () => {
    const { page, t } = setup({ autoplay: false });
    page.queue.automixItems.push({ videoId: "synthTail000" });
    await waitInitial(t, ["queueAutoplay"]);
    expect(playback(t).autoplay).toBe(false);
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.queueAutoplay).toBe("supported");

    const { id: enable, from } = await dispatchCommand(t, "setAutoplay", { autoplay: true });
    await waitPlayback(t, from, (p) => p.autoplay === true, "autoplay=true");
    expect(ack(t, enable)).toMatchObject({ dispatched: true });
    expect(page.toggleAutoplay).toHaveBeenCalledTimes(1);
    expect(playback(t).autoplay).toBe(true);

    const { id: alreadyEnabled } = await dispatchObserved(t, "setAutoplay", { autoplay: true });
    await settle(); // Nothing else happens: the matching setting is not clicked again.
    expect(ack(t, alreadyEnabled)).toMatchObject({ dispatched: true });
    expect(page.toggleAutoplay).toHaveBeenCalledTimes(1);

    page.toggleAutoplay.mockImplementationOnce(() => {});
    const { id: ignored } = await dispatchObserved(t, "setAutoplay", { autoplay: false });
    await settle(); // Nothing else happens: the ignored click leaves autoplay on.
    expect(ack(t, ignored)).toMatchObject({ dispatched: true });
    expect(playback(t).autoplay).toBe(true);
  });

  it("keeps unavailable settings unknown and cannot click a disabled or unpaired switch", async () => {
    const { page, t } = setup({ autoplay: false });
    await waitInitial(t, ["queueAutoplay"]);
    const toggle = page.doc.getElementById("automix")!;
    toggle.setAttribute("aria-disabled", "true");
    const { id: disabled } = await dispatchCommand(t, "setAutoplay", { autoplay: true });
    expect(ack(t, disabled)?.error?.code).toBe("UNSUPPORTED_CAPABILITY");

    toggle.removeAttribute("aria-disabled");
    toggle.setAttribute("aria-checked", "true");
    const { id: disagreement } = await dispatchCommand(t, "setAutoplay", { autoplay: true });
    expect(ack(t, disagreement)?.error?.code).toBe("UNSUPPORTED_CAPABILITY");

    toggle.removeAttribute("aria-checked");
    page.queueStore.getState = () => ({ queue: { ...page.queue } });
    const { id: unpaired } = await dispatchCommand(t, "setAutoplay", { autoplay: true });
    expect(ack(t, unpaired)?.error?.code).toBe("UNSUPPORTED_CAPABILITY");

    page.queue.autoplay = undefined;
    page.queue.automixItems.push({ videoId: "synthTail000" });
    await requestSnapshot(t);
    expect(t.last<BridgeReadyPayload>("bridge.ready")!.capabilities.queueAutoplay).toBe("unsupported");
    expect(playback(t).autoplay ?? null).toBeNull();
    await settle(); // Nothing else happens: no refused command clicks the switch later.
    expect(page.toggleAutoplay).not.toHaveBeenCalled();
  });
});

describe("bridge collection playback", () => {
  const target = { videoId: "fixture-song", playlistId: "OLAK-fixture", startIndex: 1 };
  const setId = "set-target";

  function watchNextPanel(contents: unknown[], playlistId = target.playlistId) {
    return {
      currentVideoEndpoint: { watchEndpoint: { videoId: target.videoId, playlistId, index: 0, playlistSetVideoId: "set-old" } },
      contents: { singleColumnMusicWatchNextResultsRenderer: { tabbedRenderer: { watchNextTabbedResultsRenderer: {
        tabs: [{ tabRenderer: { content: { musicQueueRenderer: { content: { playlistPanelRenderer: { playlistId, contents } } } } } }],
      } } } },
    };
  }

  it("uses the exact native watch endpoint and follows the observed album order", async () => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: target.videoId, playlistId: "PL-other", watchIndex: 0 },
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1 },
      { videoId: "next-song", playlistId: target.playlistId, watchIndex: 2 },
    ] });
    page.navigate.mockImplementationOnce(() => {
      page.loadIndex(1);
      page.player.state = -1;
    });
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.

    const id = t.command("playCollection", { target });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(page.navigate).toHaveBeenCalledExactlyOnceWith({
      watchEndpoint: { videoId: target.videoId, playlistId: target.playlistId, index: 1 },
    });
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(playback(t)).toMatchObject({
      playback: "playing", playbackContext: { playlistId: target.playlistId, startIndex: 1 },
    });

    t.command("next");
    await vi.advanceTimersByTimeAsync(10);
    expect(playback(t)).toMatchObject({
      currentTrack: { videoId: "next-song" },
      playbackContext: { playlistId: target.playlistId, startIndex: 2 },
    });
  });

  it.each([
    { playlistId: "PL-other", startIndex: 1 },
    { playlistId: target.playlistId, startIndex: 0 },
  ])("does not start the already-loaded video in a different context $playlistId/$startIndex", async (observed) => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: target.videoId, playlistId: observed.playlistId, watchIndex: observed.startIndex, psv: setId },
    ] });
    const panelItems = structuredClone(page.queue.items);
    Object.assign(panelItems[0]!.playlistPanelVideoRenderer.navigationEndpoint.watchEndpoint, {
      playlistId: target.playlistId, index: target.startIndex,
    });
    page.playerPage.playerPageWatchNextResponse = watchNextPanel(panelItems);
    // A target-like URL can precede the switch of the actual queue or never reach it.
    page.win.history.replaceState(null, "", `/watch?v=${target.videoId}&list=${target.playlistId}&index=${target.startIndex}`);
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    const id = t.command("playCollection", { target });
    await vi.advanceTimersByTimeAsync(5_100);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(page.api.playVideo).not.toHaveBeenCalled();
    expect(playback(t)).toMatchObject({ playback: "paused", playbackContext: observed });
  });

  it("waits for the requested occurrence when duplicate videos claim the same collection index", async () => {
    vi.useFakeTimers();
    const selected = { ...target, playlistEntryId: "set-requested" };
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1, psv: "set-incidental" },
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1, psv: selected.playlistEntryId },
    ] });
    page.queue.items[1]!.playlistPanelVideoRenderer.navigationEndpoint.watchEndpoint.playlistSetVideoId = selected.playlistEntryId;
    page.navigate.mockImplementationOnce(() => {
      setTimeout(() => { page.loadIndex(1); page.player.state = -1; }, 1_000);
    });
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    const id = t.command("playCollection", { target: selected });
    await vi.advanceTimersByTimeAsync(900);
    expect(page.navigate).toHaveBeenCalledExactlyOnceWith({
      watchEndpoint: { videoId: target.videoId, playlistId: target.playlistId, index: 1, playlistSetVideoId: selected.playlistEntryId },
    });
    expect(page.api.playVideo).not.toHaveBeenCalled();
    expect(playback(t).playbackContext).toEqual({ playlistId: target.playlistId, startIndex: 1, playlistEntryId: "set-incidental" });

    await vi.advanceTimersByTimeAsync(1_000);
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(playback(t)).toMatchObject({
      playback: "playing",
      playbackContext: { playlistId: target.playlistId, startIndex: 1, playlistEntryId: selected.playlistEntryId },
    });
  });

  it("withholds a conflicting occurrence identity and does not start playback from either copy", async () => {
    vi.useFakeTimers();
    const selected = { ...target, playlistEntryId: "set-renderer" };
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1, psv: selected.playlistEntryId },
    ] });
    page.queue.items[0]!.playlistPanelVideoRenderer.navigationEndpoint.watchEndpoint.playlistSetVideoId = "set-conflict";
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    const id = t.command("playCollection", { target: selected });
    await vi.advanceTimersByTimeAsync(5_100);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(page.api.playVideo).not.toHaveBeenCalled();
    expect(playback(t)).toMatchObject({ playback: "paused" });
    expect(playback(t).playbackContext).toEqual({ playlistId: target.playlistId, startIndex: 1 });
  });

  it("keeps the context unknown when the selected renderer endpoint disagrees with playback", async () => {
    const { page, t } = setup({ songs: [{ videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1 }] });
    const renderer = page.queue.items[0]!.playlistPanelVideoRenderer;
    delete renderer.playlistSetVideoId;
    renderer.navigationEndpoint.watchEndpoint.playlistSetVideoId = "set-endpoint";
    await waitInitial(t);
    expect(playback(t).playbackContext).toEqual({ playlistId: target.playlistId, startIndex: 1, playlistEntryId: "set-endpoint" });
    renderer.navigationEndpoint.watchEndpoint.videoId = "different-video";
    await requestSnapshot(t);
    expect(playback(t).playbackContext).toBeNull();

    const { id: incomplete } = await dispatchCommand(t, "playCollection", { target: { videoId: target.videoId, playlistId: target.playlistId } });
    expect(ack(t, incomplete)?.error?.code).toBe("INVALID_PAYLOAD");
    await settle(); // Nothing else happens: the incomplete target never navigates.
    expect(page.navigate).not.toHaveBeenCalled();
  });

  it("recovers exact collection playback from a unique WatchNext row while the top endpoint is stale", async () => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 0, psv: "set-old" },
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1, psv: setId },
      { videoId: "next-song", playlistId: target.playlistId, watchIndex: 2, psv: "set-next" },
    ] });
    page.playerPage.playerPageWatchNextResponse = watchNextPanel(structuredClone(page.queue.items).reverse());
    for (const { playlistPanelVideoRenderer: row } of page.queue.items.slice(1)) {
      delete row.navigationEndpoint.watchEndpoint.playlistId;
      delete row.navigationEndpoint.watchEndpoint.index;
    }
    page.navigate.mockImplementationOnce(() => {
      page.loadIndex(1);
      page.player.state = -1;
    });
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.

    const id = t.command("playCollection", { target: { ...target, playlistEntryId: setId } });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(playback(t)).toMatchObject({
      playback: "playing", playbackContext: { playlistId: target.playlistId, startIndex: 1, playlistEntryId: setId },
    });

    t.command("next");
    await vi.advanceTimersByTimeAsync(10);
    expect(playback(t)).toMatchObject({
      currentTrack: { videoId: "next-song" },
      playbackContext: { playlistId: target.playlistId, startIndex: 2, playlistEntryId: "set-next" },
    });
  });

  it("takes the navigated context from the current endpoint while the queue still selects an older row", async () => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: "older-song", playlistId: target.playlistId, watchIndex: 2, psv: "set-older" },
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1, psv: setId },
    ] });
    page.playerPage.playerPageWatchNextResponse = {
      ...watchNextPanel([]),
      currentVideoEndpoint: { watchEndpoint: { videoId: target.videoId, playlistId: target.playlistId, index: 1, playlistSetVideoId: setId } },
    };
    // The player loads the target, but the queue keeps selecting the row it showed before.
    page.navigate.mockImplementationOnce(() => {
      page.player.videoId = target.videoId;
      page.player.state = -1;
    });
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    const id = t.command("playCollection", { target });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(playback(t)).toMatchObject({
      playback: "playing", playbackContext: { playlistId: target.playlistId, startIndex: 1, playlistEntryId: setId },
    });

    // Once the queue selects the playing row, the row is the source again.
    page.loadIndex(1);
    page.playerPage.playerPageWatchNextResponse = watchNextPanel([]);
    t.snapshotRequest();
    await vi.advanceTimersByTimeAsync(10);
    expect(playback(t).playbackContext).toEqual({ playlistId: target.playlistId, startIndex: 1, playlistEntryId: setId });
  });

  it.each([
    "ambiguous panel row", "contradictory panel list", "wrong panel video", "missing panel index", "conflicting panel occurrence",
    "different selected occurrence", "no independent selected occurrence", "contradictory selected index", "contradictory selected list",
  ])("keeps a WatchNext context unverified with %s", async (caseName) => {
    const { page, t } = setup({ songs: [
      { videoId: target.videoId, playlistId: target.playlistId, watchIndex: 1, psv: setId },
    ] });
    const selected = page.queue.items[0]!.playlistPanelVideoRenderer;
    const panelItems = structuredClone(page.queue.items);
    const panelRow = panelItems[0]!.playlistPanelVideoRenderer;
    const panelEndpoint = panelRow.navigationEndpoint.watchEndpoint;
    const selectedEndpoint = selected.navigationEndpoint.watchEndpoint;
    delete selectedEndpoint.playlistId;
    delete selectedEndpoint.index;

    if (caseName === "ambiguous panel row") panelItems.push(structuredClone(panelItems[0]!));
    if (caseName === "wrong panel video") panelEndpoint.videoId = "different-video";
    if (caseName === "missing panel index") delete panelEndpoint.index;
    if (caseName === "conflicting panel occurrence") panelEndpoint.playlistSetVideoId = "set-other";
    if (caseName === "different selected occurrence") selected.playlistSetVideoId = "set-other";
    if (caseName === "no independent selected occurrence") {
      delete selected.playlistSetVideoId;
      selectedEndpoint.playlistSetVideoId = setId;
    }
    if (caseName === "contradictory selected index") selectedEndpoint.index = 7;
    if (caseName === "contradictory selected list") selectedEndpoint.playlistId = "PL-wrong";
    page.playerPage.playerPageWatchNextResponse = watchNextPanel(
      panelItems, caseName === "contradictory panel list" ? "PL-wrong" : target.playlistId,
    );

    await waitInitial(t);
    expect(playback(t).playbackContext).toBeNull();
  });
});

describe("bridge controls", () => {
  it("sets shuffle and repeat to absolute targets through the page controls and observes them", async () => {
    const { page, t } = setup();
    await waitInitial(t, ["playPause", "shuffle", "repeat"]);
    const r1 = await dispatchCommand(t, "setRepeat", { repeat: "one" });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).repeat === "one",
      "repeat=one",
      r1.from
    );
    expect(page.queue.repeatMode).toBe("ONE");
    expect(playback(t).repeat).toBe("one");

    const r2 = await dispatchCommand(t, "setRepeat", { repeat: "all" });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).repeat === "all",
      "repeat=all",
      r2.from
    );
    expect(page.queue.repeatMode).toBe("ALL");

    const s1 = await dispatchCommand(t, "setShuffle", { shuffle: true });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).shuffle === true,
      "shuffle=true",
      s1.from
    );
    expect(playback(t).shuffle).toBe(true);
    const { id: again } = await dispatchCommand(t, "setShuffle", { shuffle: true });
    expect(ack(t, again)).toMatchObject({ dispatched: true });
    expect(page.queue.shuffleEnabled).toBe(true);
  });

  it("converts volume between the protocol's 0..1 and the player API's 0..100", async () => {
    const { page, t } = setup();
    page.player.volume = 65;
    await waitInitial(t, ["playPause", "volume"]);
    expect(playback(t).volume).toBe(0.65);
    const v1 = await dispatchCommand(t, "setVolume", { volume: 0.35 });
    await t.waitFor(
      (e) =>
        e.kind === "bridge.playbackObserved" &&
        (e.payload as BridgePlaybackObservedPayload).volume === 0.35,
      "volume=0.35",
      v1.from
    );
    expect(page.api.setVolume).toHaveBeenCalledWith(35);
    expect(playback(t).volume).toBe(0.35);
  });

  it("plays targets through in-page navigation with validated ids only", async () => {
    const { page, t } = setup();
    page.navigate.mockImplementation((ep: { watchEndpoint: { videoId: string } }) => {
      page.player.videoId = ep.watchEndpoint.videoId;
      page.player.state = 1;
    });
    await waitInitial(t);
    const { id: ok } = await dispatchCommand(t, "playTarget", {
      target: { videoId: "songTTTTTTT", playlistId: "RDAMVMsongTTTTTTT", startIndex: 2 },
    });
    expect(ack(t, ok)).toMatchObject({ dispatched: true });
    expect(page.navigate).toHaveBeenCalledWith({
      watchEndpoint: { videoId: "songTTTTTTT", playlistId: "RDAMVMsongTTTTTTT", index: 2 },
    });
    const errFrom = t.sent.length;
    const bad = t.command("playTarget", { target: { videoId: "x/../../evil" } });
    await t.waitFor((e) => e.kind === "bridge.error", "INVALID_PAYLOAD error", errFrom);
    expect(ack(t, bad)).toBeUndefined();
    expect(t.last<{ code: string }>("bridge.error")?.code).toBe("INVALID_PAYLOAD");
    await settle(); // Nothing else happens: the invalid id never navigates.
    expect(page.navigate).toHaveBeenCalledTimes(1);
  });

  it("starts a paused player when playTarget names the video that is already loaded", async () => {
    const { page, t } = setup();
    await waitInitial(t);
    // The page answers navigate only after its unstarted-load recovery (about 800 ms).
    const { from } = await dispatchCommand(t, "playTarget", { target: { videoId: page.player.videoId } }, undefined, 4_000);
    await waitPlayback(t, from, (p) => p.playback === "playing", "playing after playTarget");
    expect(page.navigate).toHaveBeenCalledTimes(1);
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(playback(t).playback).toBe("playing");
  });

  it("starts the target once the page has loaded it paused after navigation", async () => {
    const { page, t } = setup();
    page.navigate.mockImplementation(() => {
      setTimeout(() => {
        page.player.videoId = "songNNNNNNN";
        page.player.state = -1;
      }, 150);
    });
    await waitInitial(t);
    const from = t.sent.length;
    const id = t.command("playTarget", { target: { videoId: "songNNNNNNN" } });
    // Deliberate window: the load has sat at -1 for well under the 800 ms recovery delay.
    await flush(400);
    expect(page.api.playVideo).not.toHaveBeenCalled(); // -1 is also the normal transient while loading
    await t.waitForAck(id, from, 4_000);
    expect(ack(t, id)).toMatchObject({ dispatched: true });
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(page.player.state).toBe(1);
  });

  it("starts the next track when a skip from playback leaves it unstarted, unless a newer command came", async () => {
    const { page, t } = setup();
    const next = page.doc.querySelector(".next-button")!;
    next.addEventListener("click", () => { page.player.state = -1; });
    Object.assign(page.player, { state: 1 });
    await waitInitial(t, ["playPause", "next"]);
    // The recovery start runs in the page after the ack, possibly after the bridge's last
    // follow-up probe, so wait for the page call and then read the page back.
    const started = new Promise<void>((resolve) => {
      page.api.playVideo.mockImplementationOnce(() => {
        page.player.state = 1;
        resolve();
      });
    });
    await dispatchCommand(t, "next");
    await started;
    await requestSnapshot(t);
    expect(playback(t).currentTrack?.videoId).toBe("song00000b2");
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(playback(t).playback).toBe("playing");

    // A pause right after the skip (e.g. the core's skip-while-paused pause) wins.
    await dispatchCommand(t, "next");
    await flush(50);
    await dispatchCommand(t, "pause");
    // Nothing else happens: the 800 ms recovery delay passes without a start.
    await flush(1_000);
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);

    // A skip from pause does not start playback.
    page.player.state = 2;
    await dispatchCommand(t, "next");
    // Nothing else happens: the 800 ms recovery delay passes without a start.
    await flush(1_000);
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
  });

  it("does not recover an unstarted skip after trusted playback input in the official page", async () => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: "songAAAAAAA" }, { videoId: "songBBBBBBB" }, { videoId: "songCCCCCCC" },
    ] });
    const playPause = page.doc.createElement("button");
    playPause.id = "play-pause-button";
    page.doc.querySelector("ytmusic-player-bar")!.append(playPause);
    const add = vi.spyOn(page.win, "addEventListener");
    installPageShim(page.win);
    // jsdom-created clicks cannot carry isTrusted. Exercise the installed capture callback.
    const capture = add.mock.calls.find(([type, , options]) => type === "click" && options === true)?.[1];
    page.doc.querySelector(".next-button")!.addEventListener("click", () => { page.player.state = -1; });
    page.player.state = 1;
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    t.command("next");
    await vi.advanceTimersByTimeAsync(100);
    expect(page.queue.selectedItemIndex).toBe(1);
    if (typeof capture === "function") {
      capture.call(page.win, { isTrusted: true, type: "click", composedPath: () => [playPause] } as unknown as Event);
    }
    page.player.state = 2;
    await vi.advanceTimersByTimeAsync(5_100);
    expect(page.api.playVideo).not.toHaveBeenCalled();
    expect(page.player.state).toBe(2);

    page.player.state = 1;
    t.command("next");
    await vi.advanceTimersByTimeAsync(100);
    playPause.click();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(page.queue.selectedItemIndex).toBe(2);
  });

  it("does not transfer a skip's recovery to a different load or keep it after shim replacement", async () => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null, songs: [
      { videoId: "songAAAAAAA" }, { videoId: "songBBBBBBB" }, { videoId: "songCCCCCCC" },
    ] });
    page.doc.querySelector(".next-button")!.addEventListener("click", () => { page.player.state = -1; });
    page.player.state = 1;
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    t.command("next");
    await vi.advanceTimersByTimeAsync(500);
    expect(page.queue.selectedItemIndex).toBe(1);
    page.loadIndex(2);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(page.api.playVideo).not.toHaveBeenCalled();

    page.loadIndex(0);
    page.player.state = 1;
    t.command("next");
    await vi.advanceTimersByTimeAsync(100);
    expect(page.queue.selectedItemIndex).toBe(1);
    installPageShim(page.win);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(page.api.playVideo).not.toHaveBeenCalled();
  });

  it("returns the cached ack for a repeated commandId without running it twice", async () => {
    const { page, t } = setup();
    await waitInitial(t);
    await dispatchCommand(t, "play", null, "cmd-dup");
    await dispatchCommand(t, "play", null, "cmd-dup");
    await settle(); // Nothing else happens: the retransmit does not reach the page.
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(t.of<BridgeCommandAckPayload>("bridge.commandAck").filter((a) => a.commandId === "cmd-dup")).toHaveLength(2);
  });

  it("lets the core reuse an expired commandId for the same operation and arguments", async () => {
    vi.useFakeTimers();
    const { page, t } = setup({ datasyncId: null });
    await vi.advanceTimersByTimeAsync(0);
    await waitInitial(t); // Already sent: without an account id the first probe needs no real async work.
    t.command("pause", null, "cmd-expiring");
    await vi.advanceTimersByTimeAsync(0);
    expect(page.api.pauseVideo).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000 + 1);
    t.command("pause", null, "cmd-expiring");
    await vi.advanceTimersByTimeAsync(0);
    expect(page.api.pauseVideo).toHaveBeenCalledTimes(2);
  });

  it("runs a later phase of a composite command that reuses the commandId", async () => {
    const { page, t } = setup();
    Object.assign(page.player, { state: 2, timeSec: 1 });
    await waitInitial(t, ["playPause", "seek", "next"]);
    // Skip while paused: the core sends next, then a pause under the same commandId.
    const skip = await dispatchCommand(t, "next", null, "cmd-skip");
    await waitPlayback(t, skip.from, (p) => p.currentTrack?.videoId === "song00000b2", "skipped track");
    page.player.state = 1;
    await dispatchCommand(t, "pause", null, "cmd-skip");
    expect(page.api.pauseVideo).toHaveBeenCalledTimes(1);
    expect(page.player.state).toBe(2);
    // Stop while playing: pause, then seek to 0, both under the stop's commandId.
    page.player.state = 1;
    page.player.timeSec = 42;
    const tid = playback(t).trackInstanceId;
    await dispatchCommand(t, "pause", { trackInstanceId: tid }, "cmd-stop");
    const stop = await dispatchCommand(t, "seek", { trackInstanceId: tid, positionMs: 0 }, "cmd-stop");
    await waitPlayback(t, stop.from, (p) => p.playback === "paused" && p.positionMs === 0, "stopped at 0");
    expect(page.api.seekTo).toHaveBeenLastCalledWith(0, true);
    expect(playback(t)).toMatchObject({ playback: "paused", positionMs: 0 });
    const acks = t.of<BridgeCommandAckPayload>("bridge.commandAck");
    expect(acks.filter((a) => a.commandId === "cmd-skip" && a.dispatched)).toHaveLength(2);
    expect(acks.filter((a) => a.commandId === "cmd-stop" && a.dispatched)).toHaveLength(2);
  });

  it("sends no ack when the page shim does not answer, and does not run a retransmit again", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const page = createFakePage();
    const t = createTransport();
    const ready = new Promise<BridgeReadyPayload>((resolve) => {
      const send = t.transport.sendEnvelope;
      t.transport.sendEnvelope = (env) => {
        const result = send(env);
        if (env.kind === "bridge.ready") resolve(env.payload as BridgeReadyPayload);
        return result;
      };
    });
    const timeoutMs = 30;
    track(installBridge(t.transport, EPOCH, { windowRef: page.win, documentRef: page.doc, healthIntervalMs: 0, shimTimeoutMs: timeoutMs }));
    expect((await ready).capabilities.next).toBe("supported");
    // The page main thread is stuck: requests reach it, answers never come back in time.
    const post = page.win.postMessage.bind(page.win);
    let held = 0;
    page.win.postMessage = ((data: { direction?: string; op?: string }, origin: string) => {
      if (data?.direction === "to-page" && data.op === "next") held += 1;
      else post(data, origin);
    }) as typeof page.win.postMessage;
    t.command("next", null, "cmd-slow");
    expect(held).toBe(1);
    await vi.advanceTimersByTimeAsync(timeoutMs);
    expect(ack(t, "cmd-slow")).toBeUndefined();
    t.command("next", null, "cmd-slow");
    await vi.advanceTimersByTimeAsync(timeoutMs);
    expect(held).toBe(1);
    expect(ack(t, "cmd-slow")).toBeUndefined();
  });

  it("rejects commands bound to a stale trackInstanceId", async () => {
    const { t } = setup();
    await waitInitial(t);
    const { id } = await dispatchCommand(t, "seek", { positionMs: 1_000, trackInstanceId: "ti:old:1" });
    expect(ack(t, id)?.error?.code).toBe("STALE_CONTEXT");
  });
});

describe("bridge install boundaries", () => {
  it("refuses installation outside the music.youtube.com top frame", () => {
    const auth = createFakePage({ url: "https://accounts.google.com/signin" });
    const t = createTransport();
    expect(installBridge(t.transport, EPOCH, { windowRef: auth.win, documentRef: auth.doc }).installed).toBe(false);
    expect(t.sent).toHaveLength(0);
  });

  it("reinstalls idempotently, even from a second injection of the bundle, so one command reaches the page once", async () => {
    const page = createFakePage();
    const t = createTransport();
    const opts = { windowRef: page.win, documentRef: page.doc, healthIntervalMs: 0 };
    track(installBridge(t.transport, "epoch-1", opts));
    vi.resetModules();
    const reinjected = await import("../src/bridge.js");
    track(reinjected.installBridge(t.transport, "epoch-2", opts));
    expect(t.listenerCount()).toBe(1);
    installPageShim(page.win);
    await waitInitial(t);
    await dispatchCommand(t, "play");
    await settle(); // Nothing else happens: no second bridge forwards the command again.
    expect(page.api.playVideo).toHaveBeenCalledTimes(1);
    expect(t.of<BridgeCommandAckPayload>("bridge.commandAck").every((a) => a.bridgeEpoch === "epoch-2")).toBe(true);
  });

  it("the page shim answers only same-window, same-origin messages on its channel", async () => {
    const page = createFakePage();
    const replies: unknown[] = [];
    page.win.addEventListener("message", (e) => {
      const d = (e as MessageEvent).data as { direction?: string };
      if (d?.direction === "from-page") replies.push(d);
    });
    const send = (data: unknown, init: { origin?: string; source?: unknown }) =>
      page.win.dispatchEvent(new page.dom.window.MessageEvent("message", { data, origin: init.origin, source: init.source as never }));
    const req = { channel: PAGE_SHIM_CHANNEL, direction: "to-page", requestId: "r1", op: "play" };
    send(req, { origin: "https://evil.example", source: page.win });
    send(req, { origin: "https://music.youtube.com", source: null });
    send({ ...req, op: "eval" }, { origin: "https://music.youtube.com", source: page.win });
    send({ ...req, channel: "other" }, { origin: "https://music.youtube.com", source: page.win });
    // Nothing else happens: the shim answers none of the four messages.
    await settle();
    expect(replies).toHaveLength(0);
    expect(page.api.playVideo).not.toHaveBeenCalled();
  });

  it("reports honest unsupported capabilities and explicit errors when the player API is missing", async () => {
    const page = createFakePage();
    page.doc.getElementById("movie_player")!.removeAttribute("id");
    const t = createTransport();
    track(installBridge(t.transport, EPOCH, { windowRef: page.win, documentRef: page.doc, healthIntervalMs: 0 }));
    await waitInitial(t, []);
    const caps = t.last<BridgeReadyPayload>("bridge.ready")!.capabilities;
    expect([caps.playPause, caps.seek, caps.next, caps.volume, caps.queueEnqueue, caps.queueReorder]).toEqual([
      "unsupported",
      "unsupported",
      "unsupported",
      "unsupported",
      "unsupported",
      "unsupported",
    ]);
    expect(t.last<BridgeHealthPayload>("bridge.health")).toMatchObject({ healthy: false });
    const { id } = await dispatchCommand(t, "play");
    expect(ack(t, id)).toMatchObject({ dispatched: false, error: { code: "UNSUPPORTED_CAPABILITY" } });
    const p = playback(t);
    expect([p.codec, p.audioMode, p.qualityLabel, p.drmProtected]).toEqual([null, null, null, null]);
  });
});
