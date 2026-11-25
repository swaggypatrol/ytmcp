import {
  FakeCoreClient,
  PROTOCOL_VERSION,
  type AppSnapshot,
  type CoreReadyPayload,
  type Envelope,
} from "@ytmcp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  applyStateDelta,
  createUiClient,
  wrapPreloadEnvelopeBridge,
  type RawPreloadUiBridge,
  type YtmcpUiBridge,
} from "../client.js";

afterEach(() => {
  vi.useRealTimers();
});

/** Raw preload bridge whose daemon "restarts" (emits a new core.ready) right after receiving `crashOn`. */
function restartingRawBridge(crashOn: Envelope["kind"]) {
  const listeners = new Set<(env: Envelope) => void>();
  const sent: Envelope[] = [];
  const raw: RawPreloadUiBridge = {
    onEnvelope: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    sendEnvelope: async (env) => {
      sent.push(env);
      if (env.kind === crashOn) {
        const ready: Envelope = {
          protocolVersion: PROTOCOL_VERSION,
          kind: "core.ready",
          coreEpoch: "core-2",
          seq: 1,
          payload: { coreEpoch: "core-2", protocolVersion: PROTOCOL_VERSION },
        };
        for (const listener of [...listeners]) listener(ready);
      }
      return { ok: true };
    },
  };
  return { raw, sent };
}

describe("requests orphaned by a core restart", () => {
  it("reports an account write as indeterminate and never replays it", async () => {
    const { raw, sent } = restartingRawBridge("ui.mutation");
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(raw) });

    const result = await client.sendMutation({
      kind: "setLike",
      videoId: "vid-1",
      liked: true,
      playlistId: null,
      confirmedByUser: true,
    });

    expect(result.outcome).toBe("indeterminate");
    expect(result.error?.code).toBe("TIMEOUT");
    expect(sent.filter((env) => env.kind === "ui.mutation")).toHaveLength(1);
  });

  it("reports a settings update as unanswered rather than refused", async () => {
    const { raw } = restartingRawBridge("ui.settingsUpdate");
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(raw) });

    const result = await client.updateSettings({ theme: "light" });

    expect(result).toMatchObject({ outcome: "failed", error: { code: "TIMEOUT" } });
  });

  it("reports a cache clear as unconfirmed rather than failed", async () => {
    const { raw } = restartingRawBridge("ui.clearCache");
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(raw) });

    await expect(client.clearCache(60_000)).resolves.toEqual({ outcome: "unconfirmed" });
  });
});

describe("resubscribe after a core restart", () => {
  const RETRY_DELAYS_MS = [100, 200] as const;
  const baseSnapshot = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;

  /** Bridge whose subscribe attempts succeed only while `failSubscribe` is false. */
  function restartableBridge() {
    const state = {
      attempts: 0,
      failSubscribe: false,
      coreEpoch: "core-1",
      restart: (_ready: CoreReadyPayload) => {},
    };
    const bridge: YtmcpUiBridge = {
      subscribeSnapshot: () => {
        state.attempts += 1;
        if (state.failSubscribe) return Promise.reject(new Error("new daemon not ready"));
        const snapshot: AppSnapshot = { ...structuredClone(baseSnapshot), mockMode: false, coreEpoch: state.coreEpoch };
        return { snapshot, unsubscribe: () => {} };
      },
      onCoreRestart: (listener) => {
        state.restart = listener;
        return () => {};
      },
      sendPlayerCommand: vi.fn(),
      queryCatalog: vi.fn(),
      queryLyrics: vi.fn(),
      updateSettings: vi.fn(),
      openPlayerWindow: vi.fn(),
    };
    const restartCore = (coreEpoch: string) => {
      state.coreEpoch = coreEpoch;
      state.restart({ coreEpoch, protocolVersion: PROTOCOL_VERSION });
    };
    return { bridge, state, restartCore };
  }

  async function connectedClient() {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const harness = restartableBridge();
    const client = createUiClient({ preloadBridge: harness.bridge, connectRetryDelaysMs: RETRY_DELAYS_MS });
    await client.connect();
    expect(client.getState().snapshot?.coreEpoch).toBe("core-1");
    return { client, ...harness };
  }

  it("retries a failed resubscribe on the bounded schedule until the new core answers", async () => {
    const { client, state, restartCore } = await connectedClient();
    state.failSubscribe = true;
    restartCore("core-2");
    await vi.advanceTimersByTimeAsync(0);
    expect(client.getState().connectionError).toBe("new daemon not ready");

    state.failSubscribe = false;
    await vi.advanceTimersByTimeAsync(RETRY_DELAYS_MS[0]);

    expect(state.attempts).toBe(3);
    expect(client.getState().snapshot?.coreEpoch).toBe("core-2");
    expect(client.getState().connectionError).toBeNull();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(state.attempts).toBe(3);
  });

  it("stops after the schedule runs out, and a new restart starts a fresh schedule", async () => {
    const { state, restartCore } = await connectedClient();
    state.failSubscribe = true;
    restartCore("core-2");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(state.attempts).toBe(1 + 1 + RETRY_DELAYS_MS.length);

    restartCore("core-3");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(state.attempts).toBe(1 + 2 * (1 + RETRY_DELAYS_MS.length));
  });

  it("stops retrying after disconnect", async () => {
    const { client, state, restartCore } = await connectedClient();
    state.failSubscribe = true;
    restartCore("core-2");
    await vi.advanceTimersByTimeAsync(0);
    client.disconnect();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(state.attempts).toBe(2);
  });
});

describe("merging a playerChanged delta", () => {
  /** A delta built the way the core sends one: brand-new objects, nothing shared with `base`. */
  function playerDelta(base: AppSnapshot, change: (player: AppSnapshot["player"]) => void) {
    const player = structuredClone(base.player);
    change(player);
    return {
      deltaType: "playerChanged" as const,
      revision: base.revision + 1,
      player,
      pending: structuredClone(base.pending),
    };
  }

  it("keeps the current track and pending objects when only the position moved", () => {
    const base = new FakeCoreClient().subscribeSnapshot(() => {}).snapshot;
    expect(base.player.currentTrack).toBeTruthy();
    const next = applyStateDelta(
      base,
      playerDelta(base, (player) => {
        player.timeAnchor = { ...player.timeAnchor, positionMs: player.timeAnchor.positionMs + 1000 };
      }),
    );
    expect(next.player).not.toBe(base.player);
    expect(next.player.timeAnchor.positionMs).toBe(base.player.timeAnchor.positionMs + 1000);
    expect(next.player.currentTrack).toBe(base.player.currentTrack);
    expect(next.pending).toBe(base.pending);
  });

  it("replaces the current track when something about it changed", () => {
    const base = new FakeCoreClient().subscribeSnapshot(() => {}).snapshot;
    const next = applyStateDelta(
      base,
      playerDelta(base, (player) => {
        player.currentTrack = { ...player.currentTrack!, isLiked: !player.currentTrack!.isLiked };
      }),
    );
    expect(next.player.currentTrack).not.toBe(base.player.currentTrack);
    expect(next.player.currentTrack?.isLiked).toBe(!base.player.currentTrack!.isLiked);
  });
});
