import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Envelope, HostHelloPayload, QueryError, SessionMaterialPayload } from "@ytmcp/protocol";
import { afterEach, describe, expect, it } from "vitest";
import { RustHostClient } from "../client.js";

const hello: HostHelloPayload = {
  appVersion: "0.1.0",
  electronVersion: "45.4.1",
  chromiumVersion: "156.0.0.0",
  hostHealth: {
    waylandNative: null,
    gpuAccelerated: null,
    sandboxEnabled: true,
    secretStoreBackend: "basic_text",
    safeStorageAvailable: false,
    trayAvailable: false,
    mprisActive: false,
    coreRestartCount: 0,
    lastError: null,
  },
  networkState: "online",
  mockMode: false,
};

/** Answers hello, then rejects every host.sessionMaterial the way ytm-daemon does after a failed identity read. */
const FAKE_DAEMON = `#!/usr/bin/env node
import readline from "node:readline";
let seq = 0;
const out = (env) => process.stdout.write(JSON.stringify({ protocolVersion: 1, coreEpoch: "core-1", seq: ++seq, ...env }) + "\\n");
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.kind === "host.hello") {
    out({ kind: "core.ready", requestId: msg.requestId, payload: { coreEpoch: "core-1", protocolVersion: 1 } });
  } else if (msg.kind === "host.sessionMaterial") {
    out({
      kind: "core.error",
      requestId: msg.requestId,
      payload: { code: "NETWORK_OFFLINE", retriable: true, retryAfterMs: null, safeMessage: "identity read failed", requestId: msg.requestId },
    });
  }
});
`;

const material = (revision: string): SessionMaterialPayload => ({
  accountEpoch: "acct-test",
  authUserIndex: null,
  channelLabel: null,
  cookiePairs: { SAPISID: "synthetic" },
  credentialRevision: revision,
});

describe("host.sessionMaterial error replies", () => {
  const dirs: string[] = [];
  afterEach(() => {
    for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
  });

  it("reports only the newest material's error to the host and never forwards it to the UI", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ytm-session-error-"));
    dirs.push(dir);
    const daemonBin = join(dir, "fake-daemon.mjs");
    writeFileSync(daemonBin, FAKE_DAEMON, { mode: 0o755 });

    const sessionErrors: QueryError[] = [];
    const uiEnvelopes: Envelope[] = [];
    const client = new RustHostClient({
      daemonBin,
      onSessionMaterialError: (err) => sessionErrors.push(err),
      onUiEnvelope: (env) => uiEnvelopes.push(env),
      onLog: () => {},
    });
    try {
      await client.start(hello);
      expect(client.sendSessionMaterial(material("cred-1"))).toBe(true);
      expect(client.sendSessionMaterial(material("cred-2"))).toBe(true);
      await expect.poll(() => sessionErrors.length).toBe(1);
      // Let the superseded request's error (answered first) and any stray frame settle.
      await new Promise((r) => setTimeout(r, 50));

      expect(sessionErrors).toHaveLength(1);
      expect(sessionErrors[0]).toMatchObject({ code: "NETWORK_OFFLINE", retriable: true });
      expect(sessionErrors[0]!.requestId).toMatch(/^session-/);
      expect(uiEnvelopes.filter((e) => e.kind === "core.error")).toEqual([]);
    } finally {
      await client.stop();
    }
  });
});
