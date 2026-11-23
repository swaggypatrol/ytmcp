import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { SessionMaterialPayload } from "@ytm-glass/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DesktopHostController } from "../../bootstrap/main.js";

const electron = vi.hoisted(() => ({
  online: false,
  gpuCompositing: undefined as string | undefined,
}));

vi.mock("electron", async () => {
  const { EventEmitter } = await import("node:events");
  return {
    app: Object.assign(new EventEmitter(), {
      getGPUFeatureStatus: () => (electron.gpuCompositing ? { gpu_compositing: electron.gpuCompositing } : {}),
      getGPUInfo: async () => ({}),
      quit: vi.fn(),
    }),
    net: { isOnline: () => electron.online },
    powerMonitor: new EventEmitter(),
    BrowserWindow: vi.fn(),
    session: {},
    shell: {},
  };
});

const { app, powerMonitor } = await import("electron");
const { createIntegratedDesktopHooks } = await import("../desktop-hooks.js");

/**
 * Logs every frame it receives, answers hello, rejects session material as retriable, and
 * crashes on the first host.networkChanged (the marker file keeps the restarted one alive).
 */
const FAKE_DAEMON = `#!/usr/bin/env node
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import readline from "node:readline";
const [logFile, crashMarker] = process.argv.slice(2);
let seq = 0;
const out = (env) => process.stdout.write(JSON.stringify({ protocolVersion: 1, coreEpoch: "core-" + process.pid, seq: ++seq, ...env }) + "\\n");
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const msg = JSON.parse(line);
  appendFileSync(logFile, JSON.stringify({ pid: process.pid, kind: msg.kind, network: msg.kind === "host.hello" ? msg.payload.networkState : msg.payload, gpu: msg.kind === "host.hello" ? msg.payload.hostHealth.gpuAccelerated : undefined, tray: msg.kind === "host.hello" ? msg.payload.hostHealth.trayAvailable : undefined }) + "\\n");
  if (msg.kind === "host.hello") {
    out({ kind: "core.ready", requestId: msg.requestId, payload: { coreEpoch: "core-" + process.pid, protocolVersion: 1 } });
  } else if (msg.kind === "host.sessionMaterial") {
    out({
      kind: "core.error",
      requestId: msg.requestId,
      payload: { code: "TIMEOUT", retriable: true, retryAfterMs: null, safeMessage: "identity read failed", requestId: msg.requestId },
    });
  } else if (msg.kind === "host.networkChanged" && !existsSync(crashMarker)) {
    writeFileSync(crashMarker, "");
    process.exit(1);
  }
});
`;

const material: SessionMaterialPayload = {
  accountEpoch: "acct-test",
  authUserIndex: null,
  channelLabel: null,
  cookiePairs: { SAPISID: "synthetic" },
  credentialRevision: "cred-1",
};

describe("integrated desktop hooks: network state and session material", () => {
  const dirs: string[] = [];
  afterEach(() => {
    vi.useRealTimers();
    for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
  });

  it("keeps the restart hello's network state current and drives the credential broker", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-desktop-hooks-"));
    dirs.push(dir);
    const daemonScript = join(dir, "fake-daemon.mjs");
    const logFile = join(dir, "frames.jsonl");
    const crashMarker = join(dir, "crashed");
    writeFileSync(daemonScript, FAKE_DAEMON);
    // RustHostClient spawns daemonBin with no arguments, so wrap the script with its args.
    const daemonBin = join(dir, "fake-daemon.sh");
    writeFileSync(
      daemonBin,
      `#!/bin/sh\nexec "${process.execPath}" "${daemonScript}" "${logFile}" "${crashMarker}"\n`,
      { mode: 0o755 }
    );
    const frames = (): Array<{ pid: number; kind: string; network: unknown }> =>
      existsSync(logFile)
        ? readFileSync(logFile, "utf8").trim().split("\n").map((line) => JSON.parse(line))
        : [];

    const broker = {
      handleDeliveryError: vi.fn(),
      handleNetworkOnline: vi.fn(),
      resendCurrent: vi.fn(async () => null),
    };
    const controller = {
      identity: { version: "0.1.0" },
      mockMode: false,
      windowManager: { getPlayerWindow: () => null, sendToPlayerBridge: vi.fn(), showMainWindow: vi.fn() },
      credentialBroker: broker,
      safeStorageStatus: { selectedBackend: "basic_text", secureStorageActive: false },
      trayAvailable: false,
      deliverCoreEnvelope: vi.fn(),
      shutdown: vi.fn(),
    } as unknown as DesktopHostController;

    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    electron.online = false;
    const host = createIntegratedDesktopHooks({ daemonBin });
    try {
      await host.attachController(controller);
      expect(frames().find((f) => f.kind === "host.hello")?.network).toBe("offline");

      // Renderer loss reaches the sidecar independently of connectivity and session retries.
      host.hooks.onBridgeLifecycle!({ event: "crashed", isMusicOrigin: false });
      await expect.poll(() => frames().find((f) => f.kind === "host.bridgeLifecycle")?.network)
        .toEqual({ event: "crashed", isMusicOrigin: false });

      // A retriable error for the session material goes to the broker.
      host.hooks.onSessionMaterial!(material);
      await expect.poll(() => broker.handleDeliveryError.mock.calls.length).toBe(1);
      expect(broker.handleDeliveryError.mock.calls[0]![0]).toMatchObject({ code: "TIMEOUT", retriable: true });

      // Going online is reported to the core and re-sends the material.
      electron.online = true;
      vi.advanceTimersByTime(5_000);
      expect(broker.handleNetworkOnline).toHaveBeenCalledTimes(1);
      await expect.poll(() => frames().some((f) => f.kind === "host.networkChanged")).toBe(true);

      // That frame crashed the core; the restarted one must hear "online", not the startup value.
      await expect.poll(() => broker.resendCurrent.mock.calls.length).toBe(1);
      const hellos = frames().filter((f) => f.kind === "host.hello");
      expect(hellos.map((f) => f.network)).toEqual(["offline", "online"]);
      expect(new Set(hellos.map((f) => f.pid)).size).toBe(2);

      // Waking up restarts the retry budget even when net.isOnline() never changed.
      expect(powerMonitor.emit("resume")).toBe(true);
      expect(broker.handleNetworkOnline).toHaveBeenCalledTimes(2);
      await expect.poll(() => frames().some((f) => f.kind === "host.powerResume")).toBe(true);
    } finally {
      await host.shutdown();
    }
    expect(powerMonitor.listenerCount("resume")).toBe(0);
    expect(app.listenerCount("gpu-info-update")).toBe(0);
  });

  it("re-announces GPU acceleration once Chromium reports GPU info after the handshake", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-desktop-hooks-"));
    dirs.push(dir);
    const daemonScript = join(dir, "fake-daemon.mjs");
    const logFile = join(dir, "frames.jsonl");
    writeFileSync(daemonScript, FAKE_DAEMON);
    const daemonBin = join(dir, "fake-daemon.sh");
    writeFileSync(
      daemonBin,
      `#!/bin/sh\nexec "${process.execPath}" "${daemonScript}" "${logFile}" "${join(dir, "crashed")}"\n`,
      { mode: 0o755 }
    );
    const hellos = (): Array<{ gpu: unknown }> =>
      existsSync(logFile)
        ? readFileSync(logFile, "utf8").trim().split("\n").map((line) => JSON.parse(line)).filter((f) => f.kind === "host.hello")
        : [];
    const controller = {
      identity: { version: "0.1.0" },
      mockMode: false,
      windowManager: { getPlayerWindow: () => null, sendToPlayerBridge: vi.fn(), showMainWindow: vi.fn() },
      credentialBroker: { handleDeliveryError: vi.fn(), handleNetworkOnline: vi.fn(), resendCurrent: vi.fn(async () => null) },
      safeStorageStatus: { selectedBackend: "basic_text", secureStorageActive: false },
      trayAvailable: false,
      deliverCoreEnvelope: vi.fn(),
      shutdown: vi.fn(),
    } as unknown as DesktopHostController;

    // Before the GPU process reports, Chromium has no compositing status yet.
    electron.gpuCompositing = undefined;
    const host = createIntegratedDesktopHooks({ daemonBin });
    try {
      await host.attachController(controller);
      await expect.poll(() => hellos().length).toBe(1);
      expect(hellos()[0]!.gpu).toBeNull();

      electron.gpuCompositing = "enabled";
      app.emit("gpu-info-update");
      await expect.poll(() => hellos().map((h) => h.gpu)).toEqual([null, true]);

      // An unchanged status is not re-sent.
      app.emit("gpu-info-update");
      await new Promise((r) => setTimeout(r, 200));
      expect(hellos()).toHaveLength(2);
    } finally {
      await host.shutdown();
    }
  });

  it("re-announces tray availability to the core when the tray host comes and goes", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-desktop-hooks-"));
    dirs.push(dir);
    const daemonScript = join(dir, "fake-daemon.mjs");
    const logFile = join(dir, "frames.jsonl");
    writeFileSync(daemonScript, FAKE_DAEMON);
    const daemonBin = join(dir, "fake-daemon.sh");
    writeFileSync(
      daemonBin,
      `#!/bin/sh\nexec "${process.execPath}" "${daemonScript}" "${logFile}" "${join(dir, "crashed")}"\n`,
      { mode: 0o755 }
    );
    const trays = (): unknown[] =>
      existsSync(logFile)
        ? readFileSync(logFile, "utf8").trim().split("\n").map((line) => JSON.parse(line))
            .filter((f) => f.kind === "host.hello").map((f) => f.tray)
        : [];
    const controller = {
      identity: { version: "0.1.0" },
      mockMode: false,
      windowManager: { getPlayerWindow: () => null, sendToPlayerBridge: vi.fn(), showMainWindow: vi.fn() },
      credentialBroker: { handleDeliveryError: vi.fn(), handleNetworkOnline: vi.fn(), resendCurrent: vi.fn(async () => null) },
      safeStorageStatus: { selectedBackend: "basic_text", secureStorageActive: false },
      trayAvailable: false,
      deliverCoreEnvelope: vi.fn(),
      shutdown: vi.fn(),
    } as { trayAvailable: boolean };

    const host = createIntegratedDesktopHooks({ daemonBin });
    try {
      await host.attachController(controller as unknown as DesktopHostController);
      await expect.poll(trays).toEqual([false]);

      controller.trayAvailable = true;
      host.hooks.onTrayAvailabilityChanged!(true);
      await expect.poll(trays).toEqual([false, true]);

      controller.trayAvailable = false;
      host.hooks.onTrayAvailabilityChanged!(false);
      await expect.poll(trays).toEqual([false, true, false]);
    } finally {
      await host.shutdown();
    }
  });

  it("delivers tray flips made while the core is down or still handshaking", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-desktop-hooks-"));
    dirs.push(dir);
    const daemonScript = join(dir, "fake-daemon.mjs");
    const logFile = join(dir, "frames.jsonl");
    writeFileSync(daemonScript, FAKE_DAEMON);
    const daemonBin = join(dir, "fake-daemon.sh");
    writeFileSync(
      daemonBin,
      `#!/bin/sh\nexec "${process.execPath}" "${daemonScript}" "${logFile}" "${join(dir, "crashed")}"\n`,
      { mode: 0o755 }
    );
    const hellos = (): Array<{ pid: number; tray: unknown }> =>
      existsSync(logFile)
        ? readFileSync(logFile, "utf8").trim().split("\n").map((line) => JSON.parse(line))
            .filter((f) => f.kind === "host.hello")
        : [];
    const controller = {
      identity: { version: "0.1.0" },
      mockMode: false,
      windowManager: { getPlayerWindow: () => null, sendToPlayerBridge: vi.fn(), showMainWindow: vi.fn() },
      credentialBroker: { handleDeliveryError: vi.fn(), handleNetworkOnline: vi.fn(), resendCurrent: vi.fn(async () => null) },
      safeStorageStatus: { selectedBackend: "basic_text", secureStorageActive: false },
      trayAvailable: false,
      deliverCoreEnvelope: vi.fn(),
      shutdown: vi.fn(),
    } as { trayAvailable: boolean };
    const flip = (available: boolean): void => {
      controller.trayAvailable = available;
      host.hooks.onTrayAvailabilityChanged!(available);
    };

    const host = createIntegratedDesktopHooks({
      daemonBin,
      clientOptions: {
        // Runs after the host's own crash handling, i.e. while the restarted core handshakes
        // with the cached hello from crash time: the flip can only update the cache.
        onUnexpectedExit: () => flip(false),
      },
    });
    try {
      // Flip while the startup handshake is in flight: delivered once the core is ready.
      const attached = host.attachController(controller as unknown as DesktopHostController);
      flip(true);
      await attached;
      await expect.poll(() => hellos().map((h) => h.tray)).toEqual([false, true]);

      // Crash (the fake daemon dies on its first networkChanged): the restarted core ends up false.
      host.rustHost.sendNetworkChanged("online");
      await expect.poll(() => new Set(hellos().map((h) => h.pid)).size).toBe(2);
      const restartedPid = hellos().at(-1)!.pid;
      await expect.poll(() => hellos().filter((h) => h.pid === restartedPid).at(-1)!.tray).toBe(false);

      // Flipping back must not be suppressed as "already announced".
      flip(true);
      await expect.poll(() => hellos().at(-1)!.tray).toBe(true);
      expect(hellos().at(-1)!.pid).toBe(restartedPid);
    } finally {
      await host.shutdown();
    }
  });
});
