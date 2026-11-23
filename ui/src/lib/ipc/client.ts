import {
  FakeCoreClient,
  PROTOCOL_VERSION,
  type AccountEpoch,
  type AgentActivityRequest,
  type AgentActivitySnapshot,
  type AppSettings,
  type AppSnapshot,
  type CatalogQueryRequest,
  type CatalogResponsePayload,
  type CommandReceipt,
  type CoreEpoch,
  type Envelope,
  type FakeScenario,
  type LyricsQueryRequest,
  type LyricsResult,
  type MessageKind,
  type MutationRequest,
  type MutationResult,
  type PlayerCommandPayload,
  type PlayerOp,
  type PlaybackStatus,
  type QueryError,
  type StateDelta,
} from "@ytm-glass/protocol";
import { activeMessages } from "../i18n/active.js";

export type DeltaUnsubscribe = () => void;

/** Receives one state delta plus the coreEpoch of the envelope that carried it (if known). */
export type DeltaListener = (delta: StateDelta, coreEpoch?: CoreEpoch | null) => void;
export type ResourceInvalidation = Pick<
  Extract<StateDelta, { deltaType: "resourceInvalidated" }>,
  "resource_kind" | "resource_id"
>;

/** Raised when a request was delivered but no response arrived in time (side effect unknown). */
export class IpcTimeoutError extends Error {
  constructor(kind: MessageKind) {
    super(`IPC request timeout for ${kind}`);
    this.name = "IpcTimeoutError";
  }
}

/** Raised when core answered a request with `core.error`. */
export class CoreRequestError extends Error {
  constructor(readonly error: QueryError) {
    super(error.safeMessage);
    this.name = "CoreRequestError";
  }
}

/** How long a cache clear may take before the UI reports it as unconfirmed. */
export const CACHE_CLEAR_CONFIRM_MS = 10_000;

/** Outcome of `YtmUiClient.clearCache`: confirmed only by core's catalog invalidation. */
export type CacheClearOutcome =
  | { readonly outcome: "confirmed" }
  | { readonly outcome: "failed"; readonly error: QueryError }
  | { readonly outcome: "unconfirmed" };

export interface SubscribeSnapshotResult {
  snapshot: AppSnapshot;
  unsubscribe: DeltaUnsubscribe;
}

/**
 * Raw Electron preload bridge exposed by `apps/desktop/preload/ui-preload.ts`.
 */
export interface RawPreloadUiBridge {
  sendEnvelope(envelope: Envelope): Promise<{ ok: boolean; code?: string; message?: string }>;
  onEnvelope(listener: (envelope: Envelope) => void): () => void;
}

/**
 * Narrow UI transport interface used by `YtmUiClient`.
 * Routes only generated `@ytm-glass/protocol` types.
 */
export interface YtmGlassUiBridge {
  subscribeSnapshot(
    listener: DeltaListener
  ): SubscribeSnapshotResult | Promise<SubscribeSnapshotResult>;
  sendPlayerCommand(cmd: PlayerCommandPayload): CommandReceipt | Promise<CommandReceipt>;
  /** `accountEpoch` is the account the UI believes is active; core rejects mismatches. */
  queryCatalog(
    req: CatalogQueryRequest,
    accountEpoch?: AccountEpoch | null
  ): CatalogResponsePayload | Promise<CatalogResponsePayload>;
  queryLyrics(req: LyricsQueryRequest): LyricsResult | Promise<LyricsResult>;
  updateSettings(settings: AppSettings): void | Promise<void>;
  openPlayerWindow(reason: string): void | Promise<void>;
  sendMutation?(
    req: MutationRequest,
    accountEpoch: AccountEpoch | null
  ): MutationResult | Promise<MutationResult>;
  agentActivity?(
    req: AgentActivityRequest,
    accountEpoch: AccountEpoch | null,
  ): AgentActivitySnapshot | Promise<AgentActivitySnapshot>;
  logout?(): void | Promise<void>;
  /**
   * Asks core to clear the local cache. Success has no reply (core emits a `catalog`
   * invalidation); a rejection carries core's error as `CoreRequestError`.
   */
  clearCache?(): void | Promise<void>;
}

declare global {
  interface Window {
    ytmGlassUi?: YtmGlassUiBridge | RawPreloadUiBridge;
  }
}

function isRawPreloadBridge(
  bridge: YtmGlassUiBridge | RawPreloadUiBridge
): bridge is RawPreloadUiBridge {
  return (
    typeof (bridge as RawPreloadUiBridge).sendEnvelope === "function" &&
    typeof (bridge as RawPreloadUiBridge).onEnvelope === "function" &&
    typeof (bridge as YtmGlassUiBridge).subscribeSnapshot !== "function"
  );
}

export function wrapPreloadEnvelopeBridge(
  raw: RawPreloadUiBridge,
  requestTimeoutMs = 15_000
): YtmGlassUiBridge {
  let seq = 0;
  let reqCounter = 0;
  let latestCoreEpoch: CoreEpoch | null = null;
  const activeDeltaListeners = new Set<DeltaListener>();
  const pendingRequests = new Map<
    string,
    {
      resolve: (env: Envelope) => void;
      reject: (err: Error) => void;
      timer: ReturnType<typeof setTimeout>;
    }
  >();

  let envelopeUnsubscribe: (() => void) | null = null;

  const ensureListener = (): void => {
    if (envelopeUnsubscribe) {
      return;
    }
    envelopeUnsubscribe = raw.onEnvelope((env: Envelope) => {
      if (env.coreEpoch) {
        latestCoreEpoch = env.coreEpoch;
      }
      if (env.kind === "core.delta") {
        const delta = env.payload as StateDelta;
        for (const listener of activeDeltaListeners) {
          listener(delta, env.coreEpoch ?? null);
        }
        return;
      }
      if (env.requestId && pendingRequests.has(env.requestId)) {
        const pending = pendingRequests.get(env.requestId)!;
        pendingRequests.delete(env.requestId);
        clearTimeout(pending.timer);
        pending.resolve(env);
        return;
      }
      // Fallback: route unsolicited core.snapshot if a subscribeSnapshot request is pending
      if (env.kind === "core.snapshot") {
        for (const [reqId, pending] of pendingRequests) {
          if (reqId.startsWith("ui-sub-")) {
            pendingRequests.delete(reqId);
            clearTimeout(pending.timer);
            pending.resolve(env);
            break;
          }
        }
      }
    });
  };

  const nextReqId = (prefix = "ui-req"): string => {
    reqCounter += 1;
    return `${prefix}-${Date.now()}-${reqCounter}`;
  };

  const sendFireAndForget = async (
    kind: MessageKind,
    payload: unknown
  ): Promise<void> => {
    const env: Envelope = {
      protocolVersion: PROTOCOL_VERSION,
      kind,
      coreEpoch: latestCoreEpoch,
      seq: ++seq,
      payload,
    };
    const ack = await raw.sendEnvelope(env);
    if (!ack.ok) {
      throw new Error(ack.message ?? ack.code ?? `IPC rejected ${kind}`);
    }
  };

  const sendRequest = async (
    kind: MessageKind,
    payload: unknown,
    reqPrefix = "ui-req",
    accountEpoch: AccountEpoch | null = null
  ): Promise<Envelope> => {
    ensureListener();
    const requestId = nextReqId(reqPrefix);
    const env: Envelope = {
      protocolVersion: PROTOCOL_VERSION,
      kind,
      coreEpoch: latestCoreEpoch,
      accountEpoch,
      requestId,
      seq: ++seq,
      payload,
    };

    const responsePromise = new Promise<Envelope>((resolve, reject) => {
      const timer = setTimeout(() => {
        pendingRequests.delete(requestId);
        reject(new IpcTimeoutError(kind));
      }, requestTimeoutMs);
      pendingRequests.set(requestId, { resolve, reject, timer });
    });

    const ack = await raw.sendEnvelope(env);
    if (!ack.ok) {
      const pending = pendingRequests.get(requestId);
      if (pending) {
        pendingRequests.delete(requestId);
        clearTimeout(pending.timer);
      }
      throw new Error(ack.message ?? ack.code ?? `IPC rejected ${kind}`);
    }
    return await responsePromise;
  };

  return {
    async subscribeSnapshot(listener: DeltaListener): Promise<SubscribeSnapshotResult> {
      // Strictly register delta listener BEFORE issuing ui.subscribeSnapshot
      ensureListener();
      activeDeltaListeners.add(listener);
      try {
        const resp = await sendRequest("ui.subscribeSnapshot", {}, "ui-sub");
        if (resp.kind === "core.error") {
          const err = resp.payload as QueryError;
          throw new Error(err.safeMessage);
        }
        const snapshot = resp.payload as AppSnapshot;
        return {
          snapshot,
          unsubscribe: () => {
            activeDeltaListeners.delete(listener);
            if (activeDeltaListeners.size === 0) {
              void sendFireAndForget("ui.unsubscribe", {}).catch(() => {});
            }
          },
        };
      } catch (err) {
        activeDeltaListeners.delete(listener);
        throw err;
      }
    },

    async sendPlayerCommand(cmd: PlayerCommandPayload): Promise<CommandReceipt> {
      const resp = await sendRequest("ui.playerCommand", cmd, "ui-cmd");
      if (resp.kind === "core.error") {
        const err = resp.payload as QueryError;
        return {
          commandId: cmd.commandId,
          op: cmd.op,
          status: "failed",
          acceptedAtMs: Date.now(),
          observedAtMs: null,
          error: err,
          deduplicated: false,
        };
      }
      return resp.payload as CommandReceipt;
    },

    async queryCatalog(
      req: CatalogQueryRequest,
      accountEpoch: AccountEpoch | null = null
    ): Promise<CatalogResponsePayload> {
      const resp = await sendRequest("ui.catalogQuery", req, "ui-cat", accountEpoch);
      if (resp.kind === "core.error") {
        return {
          resultType: "error",
          ...(resp.payload as QueryError),
        };
      }
      return resp.payload as CatalogResponsePayload;
    },

    async queryLyrics(req: LyricsQueryRequest): Promise<LyricsResult> {
      const resp = await sendRequest("ui.lyricsQuery", req, "ui-lyr");
      if (resp.kind === "core.error") {
        const err = resp.payload as QueryError;
        return {
          videoId: req.track.videoId,
          kind: "unavailable",
          source: "core-error",
          language: null,
          matchBasis: null,
          fetchedAtMs: Date.now(),
          fileOffsetMs: 0,
          userOffsetMs: 0,
          syncedLines: [],
          plainText: null,
          error: err,
        };
      }
      return resp.payload as LyricsResult;
    },

    async updateSettings(settings: AppSettings): Promise<void> {
      await sendFireAndForget("ui.settingsUpdate", settings);
    },

    async openPlayerWindow(reason: string): Promise<void> {
      await sendFireAndForget("ui.openPlayerWindow", { reason });
    },

    async sendMutation(req: MutationRequest, accountEpoch: AccountEpoch | null): Promise<MutationResult> {
      let resp: Envelope;
      try {
        resp = await sendRequest("ui.mutation", req, "ui-mut", accountEpoch);
      } catch (err) {
        // Delivered but unanswered: the remote write may or may not have happened.
        if (err instanceof IpcTimeoutError) {
          return { kind: req.kind, outcome: "indeterminate", error: transportError(err) };
        }
        throw err;
      }
      if (resp.kind === "core.error") {
        const error = resp.payload as QueryError;
        return {
          kind: req.kind,
          outcome: error.code === "TIMEOUT" ? "indeterminate" : "rejected",
          error,
        };
      }
      return resp.payload as MutationResult;
    },

    async agentActivity(req: AgentActivityRequest, accountEpoch: AccountEpoch | null): Promise<AgentActivitySnapshot> {
      const resp = await sendRequest("ui.agentActivity", req, "ui-agent", accountEpoch);
      if (resp.kind === "core.error") {
        const error = resp.payload as QueryError;
        throw Object.assign(new Error(error.safeMessage), { code: error.code });
      }
      if (resp.kind !== "core.agentActivity") throw new Error("Unexpected desktop activity response");
      return resp.payload as AgentActivitySnapshot;
    },

    async logout(): Promise<void> {
      await sendFireAndForget("ui.logout", {});
    },

    async clearCache(): Promise<void> {
      // Sent with a requestId so core's error reply (BUSY, storage failure) can be correlated.
      // Success sends no reply, so the request timing out here is expected and not an error.
      let resp: Envelope;
      try {
        resp = await sendRequest("ui.clearCache", {}, "ui-cache");
      } catch (err) {
        if (err instanceof IpcTimeoutError) return;
        throw err;
      }
      if (resp.kind === "core.error") throw new CoreRequestError(resp.payload as QueryError);
    },
  };
}

export interface ClientPendingOverlay {
  pendingCount: number;
  activeOp: PlayerOp | null;
  desiredPlayback: PlaybackStatus | null;
  desiredPositionMs: number | null;
  desiredVolume: number | null;
  lastError: QueryError | null;
}

export type ClientTransportMode = "preload" | "mock" | "unconnected";

export interface UiClientState {
  mode: ClientTransportMode;
  snapshot: AppSnapshot | null;
  pendingOverlay: ClientPendingOverlay;
  resnapshotCount: number;
  connectionError: string | null;
  /** Full-cache invalidations delivered after a successful local cache clear. */
  cacheClearCount: number;
  activityRevision: number;
}

export interface CreateUiClientOptions {
  fakeClient?: FakeCoreClient;
  mockScenario?: FakeScenario;
  enableMock?: boolean;
  preloadBridge?: YtmGlassUiBridge;
}

export type UiClientListener = (state: UiClientState) => void;

/**
 * Maps a thrown transport failure to a localized error: a delivered-but-unanswered request is
 * TIMEOUT (outcome unknown); anything else was refused before reaching core.
 */
function transportError(err: unknown): QueryError {
  return err instanceof IpcTimeoutError
    ? { code: "TIMEOUT", retriable: true, retryAfterMs: null, safeMessage: activeMessages().transport.timeout, requestId: null }
    : {
        code: "BRIDGE_INCOMPATIBLE",
        retriable: false,
        retryAfterMs: null,
        safeMessage: activeMessages().transport.refused,
        requestId: null,
      };
}

function nextCommandId(prefix = "ui-cmd"): string {
  if (typeof globalThis.crypto !== "undefined" && typeof globalThis.crypto.randomUUID === "function") {
    return `${prefix}-${globalThis.crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

export function isExplicitMockEnabled(): boolean {
  if (typeof window !== "undefined") {
    const params = new URLSearchParams(window.location.search);
    if (params.get("mock") === "1") {
      return true;
    }
  }
  try {
    const meta = import.meta as unknown as { env?: Record<string, string | undefined> };
    if (meta?.env?.VITE_YTM_MOCK === "1") {
      return true;
    }
  } catch {
    // Ignore environments without import.meta.env
  }
  return false;
}

export function applyStateDelta(snapshot: AppSnapshot, delta: StateDelta): AppSnapshot {
  switch (delta.deltaType) {
    case "playerChanged":
      return {
        ...snapshot,
        revision: delta.revision,
        player: delta.player,
        pending: delta.pending,
      };
    case "queueReplaced":
      return {
        ...snapshot,
        revision: delta.revision,
        queueSummary: delta.queue_summary,
      };
    case "authChanged":
      return {
        ...snapshot,
        revision: delta.revision,
        accountEpoch: delta.auth_state.accountEpoch ?? null,
        authState: delta.auth_state,
      };
    case "capabilitiesChanged":
      return {
        ...snapshot,
        revision: delta.revision,
        capabilities: delta.capabilities,
      };
    case "settingsChanged":
      return {
        ...snapshot,
        revision: delta.revision,
        settings: delta.settings,
      };
    case "hostHealthChanged":
      return {
        ...snapshot,
        revision: delta.revision,
        hostHealth: delta.host_health,
      };
    case "networkChanged":
      return {
        ...snapshot,
        revision: delta.revision,
        networkState: delta.network_state,
      };
    case "commandUpdated": {
      return {
        ...snapshot,
        revision: delta.revision,
        pending: {
          ...snapshot.pending,
          lastReceipt: delta.receipt,
        },
      };
    }
    case "resourceInvalidated":
    case "manualTakeover":
    case "controlOperationChanged":
      return {
        ...snapshot,
        revision: delta.revision,
      };
  }
}

function emptyPendingOverlay(): ClientPendingOverlay {
  return {
    pendingCount: 0,
    activeOp: null,
    desiredPlayback: null,
    desiredPositionMs: null,
    desiredVolume: null,
    lastError: null,
  };
}

interface LocalPendingCommand {
  op: PlayerOp;
  desiredPlayback: PlaybackStatus | null;
  desiredPositionMs: number | null;
  desiredVolume: number | null;
  acknowledged: boolean;
}

const terminalReceipt = (receipt: CommandReceipt): boolean =>
  receipt.status !== "accepted" && receipt.status !== "dispatched";

export class YtmUiClient {
  private mode: ClientTransportMode;
  private bridge: YtmGlassUiBridge | null = null;
  private fakeCore: FakeCoreClient | null = null;
  private mockScenario: FakeScenario = "normal";

  private snapshot: AppSnapshot | null = null;
  private pendingOverlay: ClientPendingOverlay = emptyPendingOverlay();
  private readonly pendingCommands = new Map<string, LocalPendingCommand>();
  private resnapshotCount = 0;
  private connectionError: string | null = null;
  private cacheClearCount = 0;
  private activityRevision = 0;
  private readonly invalidationListeners = new Set<(resource: ResourceInvalidation) => void>();

  private listeners = new Set<UiClientListener>();
  private unsubscribeBridge: DeltaUnsubscribe | null = null;
  private subscribeSeq = 0;
  private resnapshotInFlight = false;

  // Monotonic local anchor for playback position interpolation
  private localAnchorPerfMs = 0;
  private localAnchorKey = "";

  constructor(options: CreateUiClientOptions = {}) {
    if (options.fakeClient) {
      this.mode = "mock";
      this.fakeCore = options.fakeClient;
      this.bridge = this.wrapFakeCore(options.fakeClient);
    } else if (options.mockScenario || options.enableMock) {
      this.mode = "mock";
      this.mockScenario = options.mockScenario ?? "normal";
      this.fakeCore = new FakeCoreClient(this.mockScenario);
      this.bridge = this.wrapFakeCore(this.fakeCore);
    } else if (options.preloadBridge) {
      this.mode = "preload";
      this.bridge = options.preloadBridge;
    } else if (typeof window !== "undefined" && window.ytmGlassUi && !isExplicitMockEnabled()) {
      this.mode = "preload";
      this.bridge = isRawPreloadBridge(window.ytmGlassUi)
        ? wrapPreloadEnvelopeBridge(window.ytmGlassUi)
        : window.ytmGlassUi;
    } else if (isExplicitMockEnabled()) {
      this.mode = "mock";
      this.fakeCore = new FakeCoreClient("normal");
      this.bridge = this.wrapFakeCore(this.fakeCore);
    } else {
      this.mode = "unconnected";
      this.bridge = null;
      this.connectionError = "BRIDGE_NOT_DETECTED";
    }
  }

  private wrapFakeCore(fake: FakeCoreClient): YtmGlassUiBridge {
    return {
      subscribeSnapshot: (listener) => fake.subscribeSnapshot((delta) => listener(delta)),
      sendPlayerCommand: (cmd) => fake.sendPlayerCommand(cmd),
      // The mock "loading" scenario keeps catalog requests pending forever.
      queryCatalog: (req) =>
        this.mockScenario === "loading" ? new Promise(() => {}) : fake.queryCatalog(req),
      queryLyrics: (req) => fake.queryLyrics(req),
      updateSettings: (settings) => fake.updateSettings(settings),
      openPlayerWindow: () => {
        // No-op in mock mode
      },
    };
  }

  public getState(): UiClientState {
    return {
      mode: this.mode,
      snapshot: this.snapshot,
      pendingOverlay: this.pendingOverlay,
      resnapshotCount: this.resnapshotCount,
      connectionError: this.connectionError,
      cacheClearCount: this.cacheClearCount,
      activityRevision: this.activityRevision,
    };
  }

  public subscribe(listener: UiClientListener): () => void {
    this.listeners.add(listener);
    listener(this.getState());
    return () => {
      this.listeners.delete(listener);
    };
  }

  public subscribeInvalidations(listener: (resource: ResourceInvalidation) => void): DeltaUnsubscribe {
    this.invalidationListeners.add(listener);
    return () => this.invalidationListeners.delete(listener);
  }

  private invalidate(resource: ResourceInvalidation): void {
    for (const listener of this.invalidationListeners) listener(resource);
  }

  private notify(): void {
    const state = this.getState();
    for (const listener of this.listeners) {
      listener(state);
    }
  }

  private refreshAnchor(snap: AppSnapshot): void {
    const t = snap.player.timeAnchor;
    const key = `${snap.coreEpoch}:${snap.player.trackInstanceId ?? ""}:${snap.player.playback}:${t.positionMs}:${t.playbackRate}:${t.extrapolating}:${snap.revision}`;
    if (key !== this.localAnchorKey) {
      this.localAnchorKey = key;
      this.localAnchorPerfMs = typeof performance !== "undefined" ? performance.now() : Date.now();
    }
  }

  public resetInterpolationAnchor(): void {
    if (this.snapshot) {
      this.localAnchorPerfMs = typeof performance !== "undefined" ? performance.now() : Date.now();
    }
  }

  /**
   * Computes locally interpolated position in ms without mutating observed state.
   * Stops extrapolating when not playing, when core says not to, or while the window is hidden.
   * Reduced motion does not freeze the clock: position is information, not decoration.
   */
  public getInterpolatedPositionMs(options: { hidden?: boolean } = {}): number {
    const snap = this.snapshot;
    if (!snap) return 0;

    const anchor = snap.player.timeAnchor;
    const hidden = options.hidden ?? (typeof document !== "undefined" ? document.hidden : false);

    if (!anchor.extrapolating || snap.player.playback !== "playing" || hidden) {
      return anchor.positionMs;
    }

    const now = typeof performance !== "undefined" ? performance.now() : Date.now();
    const elapsed = Math.max(0, now - this.localAnchorPerfMs);
    const extrapolated = anchor.positionMs + elapsed * anchor.playbackRate;
    if (anchor.durationMs != null && anchor.durationMs > 0) {
      return Math.min(anchor.durationMs, Math.max(0, Math.round(extrapolated)));
    }
    return Math.max(0, Math.round(extrapolated));
  }

  /**
   * Listener-first subscription protocol:
   * 1. Install delta buffer before requesting snapshot.
   * 2. Once snapshot at revision R arrives, discard buffered deltas <= R.
   * 3. If any delta shows a revision gap (revision > expected + 1), drop deltas and re-snapshot.
   */
  public async connect(): Promise<void> {
    if (!this.bridge) {
      this.notify();
      return;
    }

    const currentSeq = ++this.subscribeSeq;
    this.cleanupBridgeSubscription();
    const acknowledged = [...this.pendingCommands].filter(([, cmd]) => cmd.acknowledged).map(([id]) => id);

    let snapshotReady = false;
    const buffered: StateDelta[] = [];

    const handleIncomingDelta = (delta: StateDelta, coreEpoch?: CoreEpoch | null) => {
      if (currentSeq !== this.subscribeSeq) return;
      if (!snapshotReady) {
        buffered.push(delta);
        return;
      }
      // A delta from another core instance means the core restarted: revisions restart too.
      if (coreEpoch && this.snapshot && coreEpoch !== this.snapshot.coreEpoch) {
        this.requestResnapshot();
        return;
      }
      this.processDelta(delta);
    };

    try {
      const subResult = await this.bridge.subscribeSnapshot(handleIncomingDelta);
      if (currentSeq !== this.subscribeSeq) {
        subResult.unsubscribe();
        return;
      }

      this.unsubscribeBridge = subResult.unsubscribe;
      const previousCoreEpoch: CoreEpoch | null = this.snapshot?.coreEpoch ?? null;
      this.snapshot = subResult.snapshot;
      this.activityRevision += 1;
      this.connectionError = null;
      this.refreshAnchor(subResult.snapshot);
      snapshotReady = true;

      if (previousCoreEpoch && previousCoreEpoch !== subResult.snapshot.coreEpoch) {
        this.resetPendingCommands();
      } else {
        this.applyLocalReceipt(subResult.snapshot.pending.lastReceipt);
        if (subResult.snapshot.pending.pendingCommandCount === 0) {
          for (const id of acknowledged) this.pendingCommands.delete(id);
          this.refreshPendingOverlay();
        }
      }

      if (previousCoreEpoch) this.invalidate({ resource_kind: "resync" });
      // Replay buffered deltas strictly newer than snapshot.revision
      buffered.sort((a, b) => a.revision - b.revision);
      for (const delta of buffered) {
        if (delta.revision > (this.snapshot?.revision ?? 0)) {
          const ok = this.processDelta(delta, false);
          if (!ok) {
            return;
          }
        }
      }
      this.notify();
    } catch (err) {
      this.connectionError = err instanceof Error ? err.message : String(err);
      this.notify();
    }
  }

  private processDelta(delta: StateDelta, shouldNotify = true): boolean {
    if (!this.snapshot) return false;
    const currentRev = this.snapshot.revision;

    // Drop duplicate/stale revisions
    if (delta.revision <= currentRev) {
      return true;
    }

    // Detect revision gap -> trigger re-snapshot instead of applying partial state
    if (delta.revision > currentRev + 1) {
      this.requestResnapshot();
      return false;
    }

    this.snapshot = applyStateDelta(this.snapshot, delta);
    if (delta.deltaType === "playerChanged") {
      this.refreshAnchor(this.snapshot);
      this.applyLocalReceipt(delta.pending.lastReceipt);
    } else if (delta.deltaType === "commandUpdated") {
      this.applyLocalReceipt(delta.receipt);
    } else if (delta.deltaType === "resourceInvalidated") {
      if (delta.resource_kind === "catalog") this.cacheClearCount += 1;
      this.invalidate(delta);
    } else if (delta.deltaType === "controlOperationChanged" || delta.deltaType === "manualTakeover" || delta.deltaType === "authChanged") {
      this.activityRevision += 1;
    }

    if (shouldNotify) {
      this.notify();
    }
    return true;
  }

  private requestResnapshot(): void {
    this.resnapshotCount += 1;
    if (!this.resnapshotInFlight) {
      this.resnapshotInFlight = true;
      void this.connect().finally(() => {
        this.resnapshotInFlight = false;
      });
    }
  }

  private cleanupBridgeSubscription(): void {
    if (this.unsubscribeBridge) {
      try {
        this.unsubscribeBridge();
      } catch {
        // Ignore cleanup errors
      }
      this.unsubscribeBridge = null;
    }
  }

  public disconnect(): void {
    this.subscribeSeq += 1;
    this.cleanupBridgeSubscription();
    this.resetPendingCommands();
  }

  /**
   * Explicitly switches into mock mode when user triggers preview outside Electron.
   */
  public activateMockMode(scenario: FakeScenario = "normal"): Promise<void> {
    this.mode = "mock";
    this.mockScenario = scenario;
    this.fakeCore = new FakeCoreClient(scenario);
    this.bridge = this.wrapFakeCore(this.fakeCore);
    this.connectionError = null;
    this.resetPendingCommands();
    return this.connect();
  }

  public setMockScenario(scenario: FakeScenario): void {
    if (this.mode !== "mock" || !this.fakeCore) return;
    this.mockScenario = scenario;
    this.resetPendingCommands();
    this.fakeCore.setScenario(scenario);
    // FakeCoreClient.setScenario replaces networkState/authState/capabilities/queueSummary
    // while only emitting playerChanged, so re-snapshot to sync the full projection.
    void this.connect();
  }

  public simulateRevisionGap(): void {
    if (this.mode === "mock" && this.fakeCore) {
      this.fakeCore.simulateRevisionGapOnNextDelta();
    }
  }

  public async sendPlayerCommand(
    op: PlayerOp,
    args: PlayerCommandPayload["args"] = null
  ): Promise<CommandReceipt | null> {
    if (!this.bridge) return null;

    const commandId = nextCommandId();
    // Optimistic pending overlay ONLY — observed snapshot.player is NEVER mutated here!
    this.pendingCommands.set(commandId, {
      op,
      desiredPlayback:
        op === "play"
          ? "playing"
          : op === "pause" || op === "stop"
            ? "paused"
            : op === "toggle"
              ? this.snapshot?.player.playback === "playing"
                ? "paused"
                : "playing"
              : null,
      desiredPositionMs: op === "seek" ? (args?.positionMs ?? null) : null,
      desiredVolume: op === "setVolume" ? (args?.volume ?? null) : null,
      acknowledged: false,
    });
    this.refreshPendingOverlay(null);
    this.notify();

    try {
      const receipt = await this.bridge.sendPlayerCommand({
        commandId,
        op,
        args,
      });

      if (receipt.commandId !== commandId) throw new Error("playback receipt belongs to another command");
      if (this.applyLocalReceipt(receipt)) this.notify();
      return receipt;
    } catch (err) {
      if (this.pendingCommands.delete(commandId)) {
        this.refreshPendingOverlay(transportError(err));
        this.notify();
      }
      return null;
    }
  }

  private resetPendingCommands(): void {
    this.pendingCommands.clear();
    this.pendingOverlay = emptyPendingOverlay();
  }

  private refreshPendingOverlay(error = this.pendingOverlay.lastError): void {
    const pending = [...this.pendingCommands.values()].reverse();
    this.pendingOverlay = {
      pendingCount: pending.length,
      activeOp: pending[0]?.op ?? null,
      desiredPlayback: pending.find((cmd) => cmd.desiredPlayback !== null)?.desiredPlayback ?? null,
      desiredPositionMs: pending.find((cmd) => cmd.desiredPositionMs !== null)?.desiredPositionMs ?? null,
      desiredVolume: pending.find((cmd) => cmd.desiredVolume !== null)?.desiredVolume ?? null,
      lastError: error,
    };
  }

  private applyLocalReceipt(receipt: CommandReceipt | null | undefined): boolean {
    const pending = receipt && this.pendingCommands.get(receipt.commandId);
    if (!receipt || !pending) return false;
    if (terminalReceipt(receipt)) {
      this.pendingCommands.delete(receipt.commandId);
      this.refreshPendingOverlay(receipt.error ?? null);
      return true;
    }
    pending.acknowledged = true;
    return false;
  }

  /** The account epoch the UI currently trusts; attached to account-scoped requests. */
  public currentAccountEpoch(): AccountEpoch | null {
    const snap = this.snapshot;
    return snap?.authState.accountEpoch ?? snap?.accountEpoch ?? null;
  }

  public async queryCatalog(req: CatalogQueryRequest): Promise<CatalogResponsePayload> {
    if (!this.bridge) {
      return {
        resultType: "error",
        code: "BRIDGE_INCOMPATIBLE",
        retriable: false,
        retryAfterMs: null,
        safeMessage: activeMessages().transport.noBridge,
        requestId: null,
      };
    }
    try {
      return await this.bridge.queryCatalog(req, this.currentAccountEpoch());
    } catch (err) {
      return { resultType: "error", ...transportError(err) };
    }
  }

  /**
   * Sends one explicitly confirmed account write. Never retried here: an unanswered request
   * comes back as `indeterminate` and the user is asked to check the official page.
   */
  public async sendMutation(req: MutationRequest): Promise<MutationResult> {
    if (!this.bridge?.sendMutation) {
      return {
        kind: req.kind,
        outcome: "rejected",
        error: {
          code: "UNSUPPORTED_CAPABILITY",
          retriable: false,
          retryAfterMs: null,
          safeMessage: activeMessages().transport.writesUnsupported,
          requestId: null,
        },
      };
    }
    try {
      return await this.bridge.sendMutation(req, this.currentAccountEpoch());
    } catch (err) {
      // Only a delivered-but-unanswered request is ambiguous; a refused send never reached core.
      const error = transportError(err);
      return { kind: req.kind, outcome: error.code === "TIMEOUT" ? "indeterminate" : "rejected", error };
    }
  }

  public async agentActivity(req: AgentActivityRequest): Promise<AgentActivitySnapshot> {
    if (!this.bridge?.agentActivity) throw new Error("Desktop activity is unavailable");
    return this.bridge.agentActivity(req, this.currentAccountEpoch());
  }

  public async queryLyrics(req: LyricsQueryRequest): Promise<LyricsResult> {
    if (!this.bridge) {
      return {
        videoId: req.track.videoId,
        kind: "disabled",
        source: "unconnected",
        language: null,
        matchBasis: null,
        fetchedAtMs: 0,
        fileOffsetMs: 0,
        userOffsetMs: 0,
        syncedLines: [],
        plainText: null,
        error: null,
      };
    }
    return await this.bridge.queryLyrics(req);
  }

  public async updateSettings(settings: AppSettings): Promise<void> {
    if (!this.bridge) return;
    await this.bridge.updateSettings(settings);
  }

  public async openPlayerWindow(reason: string): Promise<void> {
    if (!this.bridge) return;
    await this.bridge.openPlayerWindow(reason);
  }

  public async logout(): Promise<void> {
    if (!this.bridge?.logout) return;
    await this.bridge.logout();
  }

  /**
   * Clears the local cache and settles once the outcome is known: `confirmed` when core's
   * catalog invalidation arrives, `failed` on a core or transport error, and `unconfirmed`
   * when neither arrives within `timeoutMs`.
   */
  public clearCache(timeoutMs = CACHE_CLEAR_CONFIRM_MS): Promise<CacheClearOutcome> {
    const bridge = this.bridge;
    if (!bridge?.clearCache) {
      return Promise.resolve({
        outcome: "failed",
        error: { code: "UNSUPPORTED_CAPABILITY", retriable: false, retryAfterMs: null, safeMessage: activeMessages().gates.unsupported, requestId: null },
      });
    }
    return new Promise((resolve) => {
      let settled = false;
      const finish = (result: CacheClearOutcome) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        stopListening();
        resolve(result);
      };
      const stopListening = this.subscribeInvalidations((resource) => {
        if (resource.resource_kind === "catalog") finish({ outcome: "confirmed" });
      });
      const timer = setTimeout(() => finish({ outcome: "unconfirmed" }), timeoutMs);
      Promise.resolve()
        .then(() => bridge.clearCache!())
        .catch((err: unknown) =>
          finish({ outcome: "failed", error: err instanceof CoreRequestError ? err.error : transportError(err) })
        );
    });
  }
}

export function createUiClient(options: CreateUiClientOptions = {}): YtmUiClient {
  return new YtmUiClient(options);
}
