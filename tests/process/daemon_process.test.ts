import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  MAX_FRAME_BYTES,
  PROTOCOL_VERSION,
  parseAndValidateEnvelope,
  serializeEnvelope,
  type Envelope,
} from "@ytmcp/protocol";
import { afterEach, describe, expect, it } from "vitest";

const repoRoot = resolve(import.meta.dirname, "../..");

/** Finds the daemon under test: YTMCP_DAEMON_BIN, else the debug build `just test` makes, else a release build. */
function findDaemonBinary(): string {
  const explicit = process.env.YTMCP_DAEMON_BIN;
  if (explicit) {
    if (!existsSync(explicit)) {
      throw new Error(`YTMCP_DAEMON_BIN does not exist: ${explicit}`);
    }
    return explicit;
  }
  const targetDir = resolve(repoRoot, process.env.CARGO_TARGET_DIR ?? "target");
  const candidates = [resolve(targetDir, "debug/ytm-daemon"), resolve(targetDir, "release/ytm-daemon")];
  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  throw new Error(`ytm-daemon binary not found in ${targetDir}/debug or release; run cargo build -p ytm-daemon first`);
}

interface DaemonHarness {
  readonly binPath: string;
  readonly tempRoot: string;
  sendRaw(line: string): void;
  sendEnvelope(env: Envelope): void;
  nextEnvelope(predicate?: (env: Envelope) => boolean, timeoutMs?: number): Promise<Envelope>;
  closeStdinAndWaitExit(timeoutMs?: number): Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  kill(): void;
}

function spawnDaemonHarness(): DaemonHarness {
  const binPath = findDaemonBinary();
  const tempRoot = mkdtempSync(join(tmpdir(), "ytm-daemon-proc-"));

  const child = spawn(binPath, [], {
    cwd: repoRoot,
    env: {
      ...process.env,
      YTMCP_NO_MPRIS: "1",
      YTMCP_DATA_DIR: tempRoot,
      XDG_CONFIG_HOME: join(tempRoot, "config"),
      XDG_DATA_HOME: join(tempRoot, "data"),
      XDG_CACHE_HOME: join(tempRoot, "cache"),
      XDG_STATE_HOME: join(tempRoot, "state"),
    },
    stdio: ["pipe", "pipe", "pipe"],
  });

  const received: Envelope[] = [];
  const waiters: Array<{
    predicate: (env: Envelope) => boolean;
    resolve: (env: Envelope) => void;
    reject: (err: Error) => void;
    timer: NodeJS.Timeout;
  }> = [];

  let carry = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => {
    carry += chunk;
    let idx = carry.indexOf("\n");
    while (idx !== -1) {
      const line = carry.slice(0, idx).replace(/\r$/, "");
      carry = carry.slice(idx + 1);
      if (line.trim().length > 0) {
        const res = parseAndValidateEnvelope(line);
        if (!res.ok) {
          throw new Error(`Daemon emitted invalid envelope (${res.code}): ${res.message}\nLine: ${line}`);
        }
        const env = res.value;
        let matchedWaiter = false;
        for (let i = 0; i < waiters.length; i++) {
          const w = waiters[i]!;
          if (w.predicate(env)) {
            waiters.splice(i, 1);
            clearTimeout(w.timer);
            w.resolve(env);
            matchedWaiter = true;
            break;
          }
        }
        if (!matchedWaiter) {
          received.push(env);
        }
      }
      idx = carry.indexOf("\n");
    }
  });

  const stderrLines: string[] = [];
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk: string) => {
    stderrLines.push(chunk);
  });

  return {
    binPath,
    tempRoot,
    sendRaw(line: string) {
      child.stdin.write(line.endsWith("\n") ? line : `${line}\n`);
    },
    sendEnvelope(env: Envelope) {
      child.stdin.write(serializeEnvelope(env));
    },
    nextEnvelope(predicate = () => true, timeoutMs = 6_000): Promise<Envelope> {
      const existingIdx = received.findIndex(predicate);
      if (existingIdx !== -1) {
        return Promise.resolve(received.splice(existingIdx, 1)[0]!);
      }
      return new Promise((resolveNext, rejectNext) => {
        const timer = setTimeout(() => {
          const idx = waiters.findIndex((w) => w.timer === timer);
          if (idx !== -1) {
            waiters.splice(idx, 1);
          }
          rejectNext(
            new Error(
              `Timed out after ${timeoutMs}ms waiting for daemon envelope. Buffered kinds: ${received.map((e) => e.kind).join(", ")}. Stderr: ${stderrLines.join("")}`
            )
          );
        }, timeoutMs);
        waiters.push({ predicate, resolve: resolveNext, reject: rejectNext, timer });
      });
    },
    closeStdinAndWaitExit(timeoutMs = 5_000) {
      return new Promise((resolveExit, rejectExit) => {
        if (child.exitCode !== null || child.signalCode !== null) {
          resolveExit({ code: child.exitCode, signal: child.signalCode });
          return;
        }
        const timer = setTimeout(() => {
          rejectExit(new Error(`Daemon did not exit within ${timeoutMs}ms after stdin EOF`));
        }, timeoutMs);
        child.once("exit", (code, signal) => {
          clearTimeout(timer);
          resolveExit({ code, signal });
        });
        child.stdin.end();
      });
    },
    kill() {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
      }
      rmSync(tempRoot, { recursive: true, force: true });
    },
  };
}

function makeHostHello(seq = 1, requestId = "req-hello"): Envelope {
  return {
    protocolVersion: PROTOCOL_VERSION,
    kind: "host.hello",
    coreEpoch: null,
    accountEpoch: null,
    bridgeEpoch: null,
    requestId,
    seq,
    payload: {
      appVersion: "0.1.0",
      electronVersion: "45.4.1",
      chromiumVersion: "136.0.0.0",
      hostHealth: {
        waylandNative: true,
        gpuAccelerated: true,
        sandboxEnabled: true,
        secretStoreBackend: "kwallet6",
        safeStorageAvailable: true,
        trayAvailable: false,
        mprisActive: false,
        coreRestartCount: 0,
        lastError: null,
      },
      networkState: "online",
      mockMode: false,
    },
  };
}

describe("ytm-daemon real binary process integration", () => {
  const activeHarnesses: DaemonHarness[] = [];

  afterEach(() => {
    while (activeHarnesses.length > 0) {
      const h = activeHarnesses.pop()!;
      h.kill();
    }
  });

  it("performs host.hello -> core.ready handshake and exits cleanly (code 0) on stdin EOF", async () => {
    const daemon = spawnDaemonHarness();
    activeHarnesses.push(daemon);

    daemon.sendEnvelope(makeHostHello(1, "hello-1"));
    const readyEnv = await daemon.nextEnvelope((e) => e.kind === "core.ready");
    expect(readyEnv.protocolVersion).toBe(1);
    expect(readyEnv.requestId).toBe("hello-1");
    expect(typeof readyEnv.coreEpoch).toBe("string");
    const readyPayload = readyEnv.payload as { coreEpoch: string; protocolVersion: number };
    expect(readyPayload.protocolVersion).toBe(1);
    expect(readyPayload.coreEpoch).toBe(readyEnv.coreEpoch);

    const exit = await daemon.closeStdinAndWaitExit();
    expect(exit.code).toBe(0);
    expect(exit.signal).toBeNull();
  });

  it("rejects protocolVersion: 99, oversized (>1 MiB), and malformed NDJSON without crashing", async () => {
    const daemon = spawnDaemonHarness();
    activeHarnesses.push(daemon);

    daemon.sendEnvelope(makeHostHello(1, "hello-1"));
    const readyEnv = await daemon.nextEnvelope((e) => e.kind === "core.ready");
    const coreEpoch = readyEnv.coreEpoch!;

    // 1. protocolVersion: 99 rejection
    daemon.sendRaw(
      JSON.stringify({
        protocolVersion: 99,
        kind: "ui.ping",
        coreEpoch,
        requestId: "req-bad-ver",
        seq: 2,
        payload: {},
      })
    );
    const badVerErr = await daemon.nextEnvelope(
      (e) => e.kind === "core.error" && e.requestId === "req-bad-ver"
    );
    expect((badVerErr.payload as { code: string }).code).toBe("BRIDGE_INCOMPATIBLE");

    // 2. Oversized (> 1 MiB) line rejection
    const oversizedPayload = "x".repeat(MAX_FRAME_BYTES + 256);
    daemon.sendRaw(
      `{"protocolVersion":1,"kind":"ui.ping","seq":3,"payload":{"pad":"${oversizedPayload}"}}`
    );
    const oversizeErr = await daemon.nextEnvelope((e) => e.kind === "core.error");
    expect((oversizeErr.payload as { code: string }).code).toBe("INVALID_PAYLOAD");

    // 3. Malformed JSON rejection
    daemon.sendRaw("{not-valid-json");
    const malformedErr = await daemon.nextEnvelope((e) => e.kind === "core.error");
    expect((malformedErr.payload as { code: string }).code).toBe("INVALID_PAYLOAD");

    // 4. Daemon is still alive and responds to a valid ping
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.ping",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: "ping-after-errors",
      seq: 4,
      payload: {},
    });
    const pong = await daemon.nextEnvelope(
      (e) => e.kind === "core.pong" && e.requestId === "ping-after-errors"
    );
    expect(pong.kind).toBe("core.pong");

    const exit = await daemon.closeStdinAndWaitExit();
    expect(exit.code).toBe(0);
  });

  it("completes ui.subscribeSnapshot + bridge.ready + ui.playerCommand(toggle) -> accepted -> bridge.playbackObserved -> observed round-trip", async () => {
    const daemon = spawnDaemonHarness();
    activeHarnesses.push(daemon);

    daemon.sendEnvelope(makeHostHello(1, "hello-roundtrip"));
    const readyEnv = await daemon.nextEnvelope((e) => e.kind === "core.ready");
    const coreEpoch = readyEnv.coreEpoch!;

    // Subscribe snapshot
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.subscribeSnapshot",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: "sub-1",
      seq: 2,
      payload: {},
    });
    const snapEnv = await daemon.nextEnvelope((e) => e.kind === "core.snapshot");
    expect(snapEnv.requestId).toBe("sub-1");

    // Fake bridge registers ready + paused initial track
    const bridgeEpoch = "bridge-proc-1";
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "bridge.ready",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch,
      requestId: null,
      seq: 1,
      payload: {
        bridgeEpoch,
        bridgeVersion: "0.1.0-test",
        capabilities: {
          playPause: "supported",
          seek: "supported",
          next: "supported",
          previous: "supported",
          volume: "supported",
          shuffle: "unsupported",
          repeat: "unsupported",
          queueMirror: "supported",
          queueEnqueue: "unsupported",
          queueReorder: "unsupported",
          queueRemove: "unsupported",
          search: "unverified",
          libraryPlaylists: "unverified",
          playlistTracks: "unverified",
          albumDetail: "unverified",
          artistDetail: "unverified",
          likedSongs: "unverified",
          personalizedHome: "unsupported",
          likeMutation: "unverified",
          playlistAddMutation: "unverified",
          lyricsLrclib: "supported",
          mpris: "supported",
        },
        accountHint: null,
        loggedInObserved: false,
      },
    });

    // Wait for snapshot request emitted on new bridgeEpoch
    const snapReq = await daemon.nextEnvelope((e) => e.kind === "player.snapshotRequest");
    expect(snapReq.bridgeEpoch).toBe(bridgeEpoch);

    // Report paused state from fake bridge
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "bridge.playbackObserved",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch,
      requestId: null,
      seq: 2,
      payload: {
        bridgeEpoch,
        playback: "paused",
        trackInstanceId: "ti-proc-1",
        currentTrack: {
          videoId: "dQw4w9WgXcQ",
          title: "Process Test Track",
          artists: [{ id: null, name: "Test Artist" }],
          album: null,
          durationMs: 180_000,
          cover: null,
          availability: "available",
          versionTag: null,
          isLiked: null,
        },
        currentQueueEntryId: null,
        positionMs: 12_000,
        durationMs: 180_000,
        playbackRate: 1.0,
        sampleAgeMs: 0,
        volume: 0.8,
        muted: false,
        shuffle: null,
        repeat: null,
        codec: null,
        audioMode: null,
        qualityLabel: null,
        drmProtected: null,
      },
    });

    await daemon.nextEnvelope(
      (e) =>
        e.kind === "core.delta" &&
        (e.payload as { deltaType?: string }).deltaType === "playerChanged"
    );

    // Send ui.playerCommand(toggle) -> Core resolves Toggle from paused -> Play
    const commandId = "cmd-playpause-1";
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.playerCommand",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: "req-cmd-1",
      seq: 3,
      payload: {
        commandId,
        op: "toggle",
        args: null,
      },
    });

    const receiptEnv = await daemon.nextEnvelope(
      (e) => e.kind === "core.commandReceipt" && e.requestId === "req-cmd-1"
    );
    const receipt = receiptEnv.payload as { commandId: string; status: string };
    expect(receipt.commandId).toBe(commandId);
    expect(["accepted", "dispatched"]).toContain(receipt.status);

    // Verify player.command(play) reaches fake bridge
    const bridgeCmdEnv = await daemon.nextEnvelope((e) => e.kind === "player.command");
    const bridgeCmd = bridgeCmdEnv.payload as { commandId: string; op: string };
    expect(bridgeCmd.commandId).toBe(commandId);
    expect(bridgeCmd.op).toBe("play");

    // Fake bridge acknowledges dispatch + observes playing
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "bridge.commandAck",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch,
      requestId: null,
      seq: 3,
      payload: {
        commandId,
        bridgeEpoch,
        dispatched: true,
        error: null,
      },
    });
    daemon.sendEnvelope({
      protocolVersion: 1,
      kind: "bridge.playbackObserved",
      coreEpoch,
      accountEpoch: null,
      bridgeEpoch,
      requestId: null,
      seq: 4,
      payload: {
        bridgeEpoch,
        playback: "playing",
        trackInstanceId: "ti-proc-1",
        currentTrack: {
          videoId: "dQw4w9WgXcQ",
          title: "Process Test Track",
          artists: [{ id: null, name: "Test Artist" }],
          album: null,
          durationMs: 180_000,
          cover: null,
          availability: "available",
          versionTag: null,
          isLiked: null,
        },
        currentQueueEntryId: null,
        positionMs: 12_100,
        durationMs: 180_000,
        playbackRate: 1.0,
        sampleAgeMs: 0,
        volume: 0.8,
        muted: false,
        shuffle: null,
        repeat: null,
        codec: null,
        audioMode: null,
        qualityLabel: null,
        drmProtected: null,
      },
    });

    const observedDelta = await daemon.nextEnvelope(
      (e) =>
        e.kind === "core.delta" &&
        (e.payload as { deltaType?: string; receipt?: { commandId?: string; status?: string } })
          .deltaType === "commandUpdated" &&
        (e.payload as { receipt?: { commandId?: string; status?: string } }).receipt?.commandId ===
          commandId &&
        (e.payload as { receipt?: { status?: string } }).receipt?.status === "observed"
    );
    expect(
      (observedDelta.payload as { receipt: { status: string } }).receipt.status
    ).toBe("observed");

    const exit = await daemon.closeStdinAndWaitExit();
    expect(exit.code).toBe(0);
  });
});
