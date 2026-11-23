import { describe, expect, it } from "vitest";
import {
  FakeCoreClient,
  PROTOCOL_VERSION,
  type AppSettingsPatch,
  type Envelope,
  type MutationRequest,
  type StateDelta,
} from "@ytm-glass/protocol";
import {
  createUiClient,
  wrapPreloadEnvelopeBridge,
  type RawPreloadUiBridge,
} from "../../../ui/src/lib/ipc/client.js";

/**
 * Drives the production UI client through the real envelope adapter against a simulated host
 * that answers with the synthetic fake core. Verifies what the UI actually puts on the wire.
 */
function createHost(options: { coreEpoch?: string; answerMutation?: boolean; refuseKinds?: string[] } = {}) {
  const fake = new FakeCoreClient("normal");
  const sent: Envelope[] = [];
  const listeners = new Set<(env: Envelope) => void>();
  let coreEpoch = options.coreEpoch ?? "core-A";
  let outSeq = 0;
  const emit = (kind: Envelope["kind"], payload: unknown, requestId: string | null = null) => {
    const env: Envelope = { protocolVersion: PROTOCOL_VERSION, kind, coreEpoch, requestId, seq: ++outSeq, payload };
    for (const l of listeners) l(env);
  };
  const raw: RawPreloadUiBridge = {
    onEnvelope(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    async sendEnvelope(env) {
      sent.push(env);
      if (options.refuseKinds?.includes(env.kind)) return { ok: false, code: "IPC_REFUSED", message: "refused" };
      const id = env.requestId ?? null;
      switch (env.kind) {
        case "ui.subscribeSnapshot": {
          const snap = fake.subscribeSnapshot(() => {}).snapshot;
          emit("core.snapshot", { ...snap, coreEpoch, mockMode: false }, id);
          break;
        }
        case "ui.playerCommand":
          emit("core.commandReceipt", fake.sendPlayerCommand(env.payload as never), id);
          break;
        case "ui.catalogQuery":
          emit("core.catalogResponse", fake.queryCatalog(env.payload as never), id);
          break;
        case "ui.mutation":
          if (options.answerMutation !== false) {
            emit("core.mutationResponse", { kind: (env.payload as MutationRequest).kind, outcome: "confirmed", error: null }, id);
          }
          break;
        case "ui.settingsUpdate": {
          if (options.refuseKinds?.includes("core.error.settings")) {
            emit("core.error", { code: "STORAGE_ERROR", retriable: false, retryAfterMs: null, safeMessage: "disk full", requestId: id }, id);
          } else {
            const updated = fake.updateSettings(env.payload as AppSettingsPatch);
            emit("core.settingsUpdated", updated, id);
          }
          break;
        }
        default:
          break;
      }
      return { ok: true };
    },
  };
  return {
    raw,
    sent,
    restartCore(epoch: string) {
      coreEpoch = epoch;
    },
    emitDelta(delta: StateDelta) {
      emit("core.delta", delta);
    },
  };
}

const LIKE: MutationRequest = { kind: "setLike", videoId: "mock-vid-00001", liked: true, playlistId: null, confirmedByUser: true };

describe("UI envelope transport", () => {
  it("stamps ui.* envelopes with the subscribed coreEpoch and account requests with the accountEpoch", async () => {
    const host = createHost();
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(host.raw) });
    await client.connect();

    await client.sendPlayerCommand("play");
    const catalog = await client.queryCatalog({ kind: "libraryPlaylists" });
    expect(catalog.resultType).toBe("libraryPlaylists");
    const result = await client.sendMutation(LIKE);
    expect(result.outcome).toBe("confirmed");

    const byKind = (kind: string) => host.sent.find((e) => e.kind === kind)!;
    expect(byKind("ui.playerCommand").coreEpoch).toBe("core-A");
    expect(byKind("ui.catalogQuery")).toMatchObject({ coreEpoch: "core-A", accountEpoch: "mock-account-epoch-1" });
    expect(byKind("ui.mutation")).toMatchObject({
      coreEpoch: "core-A",
      accountEpoch: "mock-account-epoch-1",
      payload: { confirmedByUser: true, kind: "setLike" },
    });
    client.disconnect();
  });

  it("reports an unanswered mutation as indeterminate and a refused one as rejected, sending each once", async () => {
    const silent = createHost({ answerMutation: false });
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(silent.raw, 50) });
    await client.connect();
    const unknown = await client.sendMutation(LIKE);
    expect(unknown.outcome).toBe("indeterminate");
    expect(silent.sent.filter((e) => e.kind === "ui.mutation")).toHaveLength(1);
    client.disconnect();

    const refusing = createHost({ refuseKinds: ["ui.mutation"] });
    const client2 = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(refusing.raw, 50) });
    await client2.connect();
    const refused = await client2.sendMutation(LIKE);
    expect(refused.outcome).toBe("rejected");
    client2.disconnect();
  });

  it("re-subscribes when a delta arrives from a restarted core instead of dropping it as stale", async () => {
    const host = createHost();
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(host.raw) });
    await client.connect();
    const before = client.getState().snapshot!;

    host.restartCore("core-B");
    host.emitDelta({ deltaType: "networkChanged", revision: 1, network_state: "offline" });
    await new Promise((r) => setTimeout(r, 10));

    expect(client.getState().resnapshotCount).toBe(1);
    expect(client.getState().snapshot?.coreEpoch).toBe("core-B");
    expect(before.coreEpoch).toBe("core-A");
    expect(host.sent.filter((e) => e.kind === "ui.subscribeSnapshot")).toHaveLength(2);
    client.disconnect();
  });

  it("sends partial ui.settingsUpdate patches and reports core.settingsUpdated or core.error", async () => {
    const okHost = createHost();
    const okClient = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(okHost.raw) });
    await okClient.connect();
    const res = await okClient.updateSettings({ uiLanguage: "zh-CN" });
    expect(res.outcome).toBe("saved");
    if (res.outcome === "saved") {
      expect(res.settings.uiLanguage).toBe("zh-CN");
    }
    const wire = okHost.sent.find((e) => e.kind === "ui.settingsUpdate")!;
    expect(wire.payload).toEqual({ uiLanguage: "zh-CN" });
    okClient.disconnect();

    const failHost = createHost({ refuseKinds: ["core.error.settings"] });
    const failClient = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(failHost.raw) });
    await failClient.connect();
    const failRes = await failClient.updateSettings({ reduceMotion: true });
    expect(failRes).toMatchObject({ outcome: "failed", error: { code: "STORAGE_ERROR", safeMessage: "disk full" } });
    failClient.disconnect();
  });
});
