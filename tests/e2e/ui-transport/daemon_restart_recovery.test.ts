import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Envelope } from "@ytmcp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DesktopHostController } from "../../../apps/desktop/src/bootstrap/main.js";
import {
  createUiClient,
  wrapPreloadEnvelopeBridge,
  type RawPreloadUiBridge,
} from "../../../ui/src/lib/ipc/client.js";

const electron = vi.hoisted(() => ({
  online: true,
  makeMock: async () => {
    const { EventEmitter } = await import("node:events");
    const mod = {
      app: Object.assign(new EventEmitter(), {
        getGPUFeatureStatus: () => ({ gpu_compositing: "enabled" }),
        getGPUInfo: async () => ({}),
        quit: vi.fn(),
      }),
      net: { isOnline: () => electron.online },
      powerMonitor: new EventEmitter(),
      BrowserWindow: vi.fn(),
      session: {},
      shell: {},
    };
    return { ...mod, default: mod };
  },
}));

vi.mock("electron", () => electron.makeMock());
vi.mock("../../../apps/desktop/node_modules/electron/index.js", () => electron.makeMock());

const { createIntegratedDesktopHooks } = await import(
  "../../../apps/desktop/src/rust-host/desktop-hooks.js"
);
const { trayPlayingFromCoreEnvelope } = await import("../../../apps/desktop/src/bootstrap/main.js");

/**
 * Controllable fake daemon that supports ui.subscribeSnapshot, emits core.delta on
 * host.networkChanged, acknowledges new ui.playerCommand, and records every frame
 * per process PID so tests can kill the first daemon and verify post-restart recovery.
 */
const FAKE_DAEMON = `#!/usr/bin/env node
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import readline from "node:readline";

const [logFile, restartedMarker] = process.argv.slice(2);
const isRestarted = existsSync(restartedMarker);
if (!isRestarted) {
  writeFileSync(restartedMarker, String(process.pid));
}
const epoch = "core-" + process.pid;
let seq = 0;
let rev = 1;
const out = (env) =>
  process.stdout.write(
    JSON.stringify({ protocolVersion: 1, coreEpoch: epoch, seq: ++seq, ...env }) + "\\n"
  );

const makeSnapshot = (playback) => ({
  protocolVersion: 1,
  coreEpoch: epoch,
  accountEpoch: "acct-1",
  revision: rev,
  authState: {
    state: "authenticated",
    accountEpoch: "acct-1",
    accountScope: "scope-1",
    credentialRevision: "cred-1",
    channelLabel: "Test",
    playerCatalogMatched: true,
    reason: null,
  },
  player: {
    connection: "ready",
    bridgeEpoch: "bridge-1",
    playback,
    trackInstanceId: "ti-1",
    currentTrack: {
      videoId: isRestarted ? "vid-after-restart" : "vid-before-restart",
      title: isRestarted ? "Restarted Track" : "Initial Track",
      artists: [{ id: "art-1", name: "Synth" }],
      album: null,
      durationMs: 180_000,
      cover: null,
      availability: "available",
      versionTag: null,
      isLiked: null,
    },
    currentQueueEntryId: "qe-1",
    timeAnchor: {
      positionMs: 1_000,
      durationMs: 180_000,
      playbackRate: 1,
      anchorCoreMonotonicMs: 0,
      extrapolating: playback === "playing",
    },
    volume: 0.8,
    muted: false,
    shuffle: false,
    repeat: "off",
    codec: null,
    audioMode: null,
    qualityLabel: null,
    drmProtected: null,
    stoppedConfirmed: false,
  },
  queueSummary: {
    queueRevision: 1,
    currentEntryId: "qe-1",
    completeness: "complete",
    hasMore: false,
    loadedCount: 0,
    previewEntries: [],
  },
  pending: {
    pendingCommandCount: 0,
    activeOp: null,
    desiredPlayback: null,
    desiredPositionMs: null,
    desiredVolume: null,
    lastReceipt: null,
  },
  capabilities: {
    playPause: "supported",
    seek: "supported",
    next: "supported",
    previous: "supported",
    volume: "supported",
    shuffle: "supported",
    repeat: "supported",
    queueMirror: "supported",
    queueEnqueue: "supported",
    queueReorder: "supported",
    queueRemove: "supported",
    search: "supported",
    libraryPlaylists: "supported",
    playlistTracks: "supported",
    albumDetail: "supported",
    artistDetail: "supported",
    likedSongs: "supported",
    personalizedHome: "supported",
    likeMutation: "supported",
    playlistAddMutation: "supported",
    lyricsLrclib: "supported",
    mpris: "supported",
  },
  settings: {
    theme: "dark",
    reduceMotion: false,
    reduceTransparency: false,
    uiScalePercent: 100,
    autoplayOnLaunch: false,
    closeToTray: true,
    lrclibEnabled: true,
    trackNotifications: false,
    nativeWindowDecorations: true,
    uiLanguage: "en",
  },
  hostHealth: {
    waylandNative: true,
    gpuAccelerated: true,
    sandboxEnabled: true,
    secretStoreBackend: "kwallet6",
    safeStorageAvailable: true,
    trayAvailable: true,
    mprisActive: false,
    coreRestartCount: isRestarted ? 1 : 0,
    lastError: null,
  },
  networkState: "online",
  mockMode: false,
});

readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const msg = JSON.parse(line);
  appendFileSync(
    logFile,
    JSON.stringify({
      pid: process.pid,
      kind: msg.kind,
      coreEpoch: msg.coreEpoch ?? null,
      requestId: msg.requestId ?? null,
      payload: msg.payload,
    }) + "\\n"
  );
  if (msg.kind === "host.hello") {
    out({
      kind: "core.ready",
      requestId: msg.requestId,
      payload: { coreEpoch: epoch, protocolVersion: 1 },
    });
  } else if (msg.kind === "ui.subscribeSnapshot") {
    out({
      kind: "core.snapshot",
      requestId: msg.requestId,
      payload: makeSnapshot(isRestarted ? "paused" : "playing"),
    });
  } else if (msg.kind === "host.networkChanged") {
    rev += 1;
    const snap = makeSnapshot("playing");
    out({
      kind: "core.delta",
      payload: {
        deltaType: "playerChanged",
        revision: rev,
        player: snap.player,
        pending: snap.pending,
      },
    });
  } else if (msg.kind === "ui.playerCommand") {
    out({
      kind: "core.commandReceipt",
      requestId: msg.requestId,
      payload: {
        commandId: msg.payload.commandId,
        op: msg.payload.op,
        status: "observed",
        acceptedAtMs: 1,
        observedAtMs: 2,
        error: null,
        deduplicated: false,
      },
    });
  }
});
`;

describe("daemon restart recovery through host, UI client, and tray mirror", () => {
  const dirs: string[] = [];
  afterEach(() => {
    vi.useRealTimers();
    for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
  });

  it("re-subscribes UI and rebuilds tray state after killing sidecar without replaying old writes", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-restart-recovery-"));
    dirs.push(dir);
    const daemonScript = join(dir, "fake-daemon.mjs");
    const logFile = join(dir, "frames.jsonl");
    const restartedMarker = join(dir, "restarted");
    writeFileSync(daemonScript, FAKE_DAEMON);
    const daemonBin = join(dir, "fake-daemon.sh");
    writeFileSync(
      daemonBin,
      `#!/bin/sh\nexec "${process.execPath}" "${daemonScript}" "${logFile}" "${restartedMarker}"\n`,
      { mode: 0o755 }
    );

    const readFrames = (): Array<{
      pid: number;
      kind: string;
      coreEpoch: string | null;
      payload: Record<string, unknown>;
    }> =>
      existsSync(logFile)
        ? readFileSync(logFile, "utf8")
            .trim()
            .split("\n")
            .filter(Boolean)
            .map((line) => JSON.parse(line))
        : [];

    const uiListeners = new Set<(env: Envelope) => void>();
    const trayPlayingHistory: boolean[] = [];
    let trayPlaying = false;
    const broker = {
      handleDeliveryError: vi.fn(),
      handleNetworkOnline: vi.fn(),
      resendCurrent: vi.fn(async () => null),
    };
    const host = createIntegratedDesktopHooks({ daemonBin });
    const controller = {
      identity: { version: "0.1.0" },
      mockMode: false,
      windowManager: { getPlayerWindow: () => null, sendToPlayerBridge: vi.fn(), showMainWindow: vi.fn() },
      credentialBroker: broker,
      safeStorageStatus: { selectedBackend: "basic_text", secureStorageActive: false },
      trayAvailable: true,
      // Same tray mapping as the production deliverCoreEnvelope in startDesktopShell.
      deliverCoreEnvelope: (env: Envelope) => {
        const playing = trayPlayingFromCoreEnvelope(env);
        if (playing !== null) {
          trayPlaying = playing;
          trayPlayingHistory.push(trayPlaying);
        }
        for (const l of uiListeners) l(env);
      },
      shutdown: vi.fn(),
    } as unknown as DesktopHostController;

    const rawBridge: RawPreloadUiBridge = {
      onEnvelope: (listener) => {
        uiListeners.add(listener);
        return () => uiListeners.delete(listener);
      },
      sendEnvelope: async (env) => {
        host.hooks.onUiEnvelope!(env);
        return { ok: true };
      },
    };
    const uiClient = createUiClient({
      preloadBridge: wrapPreloadEnvelopeBridge(rawBridge),
    });

    try {
      await host.attachController(controller);
      await uiClient.connect();

      const snap1 = uiClient.getState().snapshot!;
      expect(snap1).toBeTruthy();
      expect(snap1.player.playback).toBe("playing");
      expect(trayPlaying).toBe(true);

      const firstHello = readFrames().find((f) => f.kind === "host.hello")!;
      const pid1 = firstHello.pid;
      const epoch1 = snap1.coreEpoch;
      expect(epoch1).toBe(`core-${pid1}`);

      // A write the old daemon never answered has an unknown effect: report it, never replay it.
      const preCrashWrite = uiClient.sendMutation({
        kind: "setLike",
        videoId: "vid-before-restart",
        liked: true,
        playlistId: null,
        confirmedByUser: true,
      });

      // Kill the sidecar daemon and wait for automatic controlled restart + UI re-subscription.
      process.kill(pid1, "SIGKILL");

      const writeRes = await preCrashWrite;
      expect(writeRes.outcome).toBe("indeterminate");

      await expect
        .poll(() => uiClient.getState().snapshot?.coreEpoch)
        .not.toBe(epoch1);

      const snap2 = uiClient.getState().snapshot!;
      const pid2 = Number(snap2.coreEpoch.replace("core-", ""));
      expect(pid2).not.toBe(pid1);
      expect(snap2.player.currentTrack?.videoId).toBe("vid-after-restart");
      expect(snap2.player.playback).toBe("paused");
      expect(trayPlaying).toBe(false);

      // A delta from the restarted daemon updates both UI snapshot and tray state.
      host.rustHost.sendNetworkChanged("offline");
      await expect.poll(() => uiClient.getState().snapshot?.player.playback).toBe("playing");
      expect(trayPlaying).toBe(true);
      expect(trayPlayingHistory).toEqual([true, false, false, true]);

      // New playback command succeeds against the new coreEpoch.
      const receipt = await uiClient.sendPlayerCommand("next");
      expect(receipt?.status).toBe("observed");

      const framesPid2 = readFrames().filter((f) => f.pid === pid2);
      expect(framesPid2.filter((f) => f.kind === "ui.subscribeSnapshot")).toHaveLength(1);
      expect(framesPid2.filter((f) => f.kind === "ui.unsubscribe")).toHaveLength(0);
      expect(framesPid2.filter((f) => f.kind === "ui.mutation")).toHaveLength(0);
      expect(framesPid2.filter((f) => f.kind === "ui.playerCommand")).toHaveLength(1);
      expect(framesPid2.find((f) => f.kind === "ui.playerCommand")?.coreEpoch).toBe(`core-${pid2}`);
    } finally {
      uiClient.disconnect();
      await host.shutdown();
    }
  });
});
