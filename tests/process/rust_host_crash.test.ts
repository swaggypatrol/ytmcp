import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Envelope, HostHelloPayload } from "@ytmcp/protocol";
import { afterEach, describe, expect, it } from "vitest";
import { RustHostClient } from "../../apps/desktop/src/rust-host/client.js";

function makeHello(): HostHelloPayload {
  return {
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
  };
}

describe("RustHostClient crash safety and protocol boundaries", () => {
  const tempDirs: string[] = [];

  afterEach(() => {
    while (tempDirs.length > 0) {
      rmSync(tempDirs.pop()!, { recursive: true, force: true });
    }
  });

  it("refuses JS fallback when sidecar binary is missing", async () => {
    const uiErrors: Envelope[] = [];
    const client = new RustHostClient({
      daemonBin: "/nonexistent/path/to/ytm-daemon",
      onUiEnvelope: (env) => {
        if (env.kind === "core.error") {
          uiErrors.push(env);
        }
      },
    });

    await expect(client.start(makeHello())).rejects.toThrow(/sidecar binary not found/i);
    expect(client.getStatus()).toBe("missing_binary");
    expect(uiErrors.length).toBe(1);
    expect((uiErrors[0]!.payload as { code: string }).code).toBe("BRIDGE_INCOMPATIBLE");
  });

  it("rejects incompatible core.ready protocolVersion without falling back", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-rust-host-badver-"));
    tempDirs.push(dir);
    const scriptPath = join(dir, "fake-daemon.mjs");
    writeFileSync(
      scriptPath,
      `#!/usr/bin/env node
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.kind === "host.hello") {
    process.stdout.write(JSON.stringify({
      protocolVersion: 99,
      kind: "core.ready",
      coreEpoch: "core-bad",
      requestId: msg.requestId,
      seq: 1,
      payload: { coreEpoch: "core-bad", protocolVersion: 99 }
    }) + "\\n");
  }
});
`,
      { mode: 0o755 }
    );

    const uiErrors: Envelope[] = [];
    const client = new RustHostClient({
      daemonBin: scriptPath,
      onUiEnvelope: (env) => {
        if (env.kind === "core.error") uiErrors.push(env);
      },
    });

    await expect(client.start(makeHello())).rejects.toThrow(/protocolVersion/i);
    expect(client.getStatus()).toBe("incompatible");
    expect(uiErrors.length).toBeGreaterThanOrEqual(1);
    await client.stop();
  });

  it("waits for a daemon that ignores graceful shutdown and SIGTERM to exit", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-rust-host-stop-"));
    tempDirs.push(dir);
    const scriptPath = join(dir, "stubborn-daemon.mjs");
    writeFileSync(
      scriptPath,
      `#!/usr/bin/env node
import { writeFileSync } from "node:fs";
import readline from "node:readline";
process.on("SIGTERM", () => writeFileSync("received-term", ""));
setInterval(() => {}, 60_000);
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.kind === "host.hello") {
    const coreEpoch = String(process.pid);
    process.stdout.write(JSON.stringify({
      protocolVersion: 1, kind: "core.ready", coreEpoch, requestId: msg.requestId, seq: 1,
      payload: { coreEpoch, protocolVersion: 1 }
    }) + "\\n");
  }
});
`,
      { mode: 0o755 }
    );
    const client = new RustHostClient({ daemonBin: scriptPath, cwd: dir });
    const pid = Number((await client.start(makeHello())).coreEpoch);
    try {
      await client.stop();
      expect(existsSync(join(dir, "received-term"))).toBe(true);
      expect(() => process.kill(pid, 0)).toThrow(expect.objectContaining({ code: "ESRCH" }));
    } catch (failure) {
      // Clean up the test process if shutdown left it running.
      try {
        process.kill(pid, "SIGKILL");
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
      throw failure;
    }
  });

  it("immediately triggers player mute/stop callback on unexpected exit and enforces max 1 controlled restart", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-rust-host-crash-"));
    tempDirs.push(dir);
    const scriptPath = join(dir, "crashing-daemon.mjs");
    writeFileSync(
      scriptPath,
      `#!/usr/bin/env node
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.kind === "host.hello") {
    const restartIdx = msg.payload?.hostHealth?.coreRestartCount ?? 0;
    process.stdout.write(JSON.stringify({
      protocolVersion: 1,
      kind: "core.ready",
      coreEpoch: "core-run-" + restartIdx,
      requestId: msg.requestId,
      seq: 1,
      payload: { coreEpoch: "core-run-" + restartIdx, protocolVersion: 1 }
    }) + "\\n");
    setTimeout(() => process.exit(42), 60);
  }
});
`,
      { mode: 0o755 }
    );

    let playerOutputMutedCount = 0;
    const exitEvents: Array<{ willRestart: boolean; restartCount: number }> = [];
    const restartedEpochs: string[] = [];
    const uiReadyEpochs: string[] = [];
    const fatalErrors: string[] = [];

    const client = new RustHostClient({
      daemonBin: scriptPath,
      maxRestarts: 1,
      onUiEnvelope: (env) => {
        if (env.kind === "core.ready") {
          uiReadyEpochs.push((env.payload as { coreEpoch: string }).coreEpoch);
        }
      },
      onUnexpectedExit: (detail) => {
        playerOutputMutedCount += 1;
        exitEvents.push({
          willRestart: detail.willRestart,
          restartCount: detail.restartCount,
        });
      },
      onControlledRestarted: (ready) => {
        restartedEpochs.push(ready.coreEpoch);
      },
      onFatalError: (err) => {
        fatalErrors.push(err.code);
      },
    });

    const firstReady = await client.start(makeHello());
    expect(firstReady.coreEpoch).toBe("core-run-0");

    // Wait long enough for 1st crash -> 1 controlled restart -> 2nd crash -> terminal crashed state.
    await new Promise((r) => setTimeout(r, 450));

    expect(playerOutputMutedCount).toBe(2);
    expect(exitEvents).toEqual([
      { willRestart: true, restartCount: 0 },
      { willRestart: false, restartCount: 1 },
    ]);
    expect(restartedEpochs).toEqual(["core-run-1"]);
    expect(uiReadyEpochs).toEqual(["core-run-1"]);
    expect(client.getRestartCount()).toBe(1);
    expect(client.getStatus()).toBe("crashed");
    expect(fatalErrors).toContain("BRIDGE_INCOMPATIBLE");

    await client.stop();
  });

  it("routes only core-role kinds from the daemon and drops ui/host kinds it must never emit", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-rust-host-roles-"));
    tempDirs.push(dir);
    const scriptPath = join(dir, "spoofing-daemon.mjs");
    writeFileSync(
      scriptPath,
      `#!/usr/bin/env node
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin });
const emit = (kind, payload, seq) =>
  process.stdout.write(JSON.stringify({ protocolVersion: 1, kind, coreEpoch: "core-r", seq, payload }) + "\\n");
rl.on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.kind === "host.hello") {
    process.stdout.write(JSON.stringify({
      protocolVersion: 1, kind: "core.ready", coreEpoch: "core-r", requestId: msg.requestId, seq: 1,
      payload: { coreEpoch: "core-r", protocolVersion: 1 }
    }) + "\\n");
    emit("ui.playerCommand", { commandId: "spoof", op: "toggle" }, 2);
    emit("host.sessionCleared", {}, 3);
    emit("player.snapshotRequest", {}, 5);
    emit("core.pong", {}, 6);
  }
});
`,
      { mode: 0o755 }
    );

    const uiKinds: string[] = [];
    const bridgeKinds: string[] = [];
    const client = new RustHostClient({
      daemonBin: scriptPath,
      onUiEnvelope: (env) => uiKinds.push(env.kind),
      onBridgeEnvelope: (env) => bridgeKinds.push(env.kind),
      onLog: () => {},
    });

    await client.start(makeHello());
    await new Promise((r) => setTimeout(r, 150));

    expect(uiKinds).toEqual(["core.pong"]);
    expect(bridgeKinds).toEqual(["player.snapshotRequest"]);
    await client.stop();
  });
});
