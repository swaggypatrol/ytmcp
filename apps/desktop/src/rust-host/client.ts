import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import {
  MAX_FRAME_BYTES,
  PROTOCOL_VERSION,
  isKindAllowedForRole,
  parseAndValidateEnvelope,
  serializeEnvelope,
  validateEnvelopeObject,
  type AssetResolvedPayload,
  type BridgeLifecyclePayload,
  type CoreReadyPayload,
  type Envelope,
  type EnvironmentConfigPayload,
  type HostEnvironmentSnapshotPayload,
  type HostHelloPayload,
  type HostWindowVisibilityPayload,
  type NetworkState,
  type OutputGateControlPayload,
  type OutputGateResultPayload,
  type QueryError,
  type QueryErrorCode,
  type SessionMaterialPayload,
} from "@ytmcp/protocol";

export type DaemonStatus =
  | "idle"
  | "starting"
  | "ready"
  | "incompatible"
  | "missing_binary"
  | "crashed"
  | "stopped";

export type MprisUiAction = "mpris:raise" | "mpris:quit";

export interface RustHostClientOptions {
  readonly daemonBin?: string;
  readonly env?: NodeJS.ProcessEnv;
  readonly cwd?: string;
  readonly handshakeTimeoutMs?: number;
  readonly maxRestarts?: number;
  readonly onUiEnvelope?: (envelope: Envelope) => void;
  readonly onBridgeEnvelope?: (envelope: Envelope) => void;
  readonly onOutputGateControl?: (
    payload: OutputGateControlPayload
  ) => Promise<OutputGateResultPayload> | OutputGateResultPayload;
  readonly onEnvironmentConfig?: (config: EnvironmentConfigPayload) => void;
  readonly onMprisAction?: (action: MprisUiAction) => void;
  readonly onUnexpectedExit?: (detail: {
    code: number | null;
    signal: NodeJS.Signals | null;
    willRestart: boolean;
    restartCount: number;
  }) => void | Promise<void>;
  readonly onControlledRestarted?: (ready: CoreReadyPayload) => void | Promise<void>;
  readonly onFatalError?: (error: QueryError) => void;
  /** The core rejected the latest host.sessionMaterial (e.g. its identity read failed). */
  readonly onSessionMaterialError?: (error: QueryError) => void;
  readonly onLog?: (
    level: "info" | "warn" | "error",
    event: string,
    detail?: Record<string, unknown>
  ) => void;
}

const DEFAULT_HANDSHAKE_TIMEOUT_MS = 10_000;
const MAX_QUEUED_STARTUP_FRAMES = 64;
const SESSION_REQUEST_PREFIX = "session-";

export function resolveDaemonBinaryPath(explicitPath?: string): string | null {
  if (explicitPath !== undefined) {
    const trimmed = explicitPath.trim();
    return trimmed !== "" && existsSync(trimmed) ? trimmed : null;
  }
  const envBin = process.env.YTMCP_DAEMON_BIN?.trim();
  if (envBin) {
    return existsSync(envBin) ? envBin : null;
  }
  const candidates = [
    ...(typeof process.resourcesPath === "string"
      ? [resolve(process.resourcesPath, "bin/ytm-daemon")]
      : []),
    "/opt/ytmcp/resources/bin/ytm-daemon",
    resolve(process.cwd(), "target/release/ytm-daemon"),
    resolve(process.cwd(), "target/debug/ytm-daemon"),
    resolve(import.meta.dirname, "../../../../target/release/ytm-daemon"),
    resolve(import.meta.dirname, "../../../../target/debug/ytm-daemon"),
  ];
  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

export class RustHostClient {
  private readonly options: RustHostClientOptions;
  private readonly handshakeTimeoutMs: number;
  private readonly maxRestarts: number;

  private child: ChildProcessWithoutNullStreams | null = null;
  private status: DaemonStatus = "idle";
  private coreEpoch: string | null = null;
  private seqCounter = 0;
  private restartCount = 0;
  private stopping = false;
  private lastHelloPayload: HostHelloPayload | null = null;

  private stdoutCarry = Buffer.alloc(0);
  private discardingOversizedLine = false;
  private discardedBytes = 0;

  private pendingStartupFrames: Envelope[] = [];
  /** requestId of the newest host.sessionMaterial; errors for older ones are superseded. */
  private latestSessionRequestId: string | null = null;
  private pendingHandshake: {
    requestId: string;
    resolve: (ready: CoreReadyPayload) => void;
    reject: (err: Error) => void;
    timer: NodeJS.Timeout;
  } | null = null;

  private readonly pendingAssetRequests = new Map<
    string,
    {
      resolve: (payload: AssetResolvedPayload) => void;
      reject: (err: Error) => void;
      timer: NodeJS.Timeout;
    }
  >();

  public constructor(options: RustHostClientOptions = {}) {
    this.options = options;
    this.handshakeTimeoutMs = options.handshakeTimeoutMs ?? DEFAULT_HANDSHAKE_TIMEOUT_MS;
    this.maxRestarts = options.maxRestarts ?? 1;
  }

  public getStatus(): DaemonStatus {
    return this.status;
  }

  public getCoreEpoch(): string | null {
    return this.coreEpoch;
  }

  public getRestartCount(): number {
    return this.restartCount;
  }

  public async start(hello: HostHelloPayload): Promise<CoreReadyPayload> {
    this.stopping = false;
    this.lastHelloPayload = hello;
    return this.spawnAndHandshake(hello);
  }

  private nextSeq(): number {
    this.seqCounter += 1;
    return this.seqCounter;
  }

  private log(level: "info" | "warn" | "error", event: string, detail?: Record<string, unknown>): void {
    if (this.options.onLog) {
      this.options.onLog(level, event, detail);
      return;
    }
    const line = JSON.stringify({
      ts: new Date().toISOString(),
      component: "rust-host",
      level,
      event,
      ...(detail ? { detail } : {}),
    });
    process.stderr.write(`${line}\n`);
  }

  private emitFatalError(code: QueryErrorCode, safeMessage: string): void {
    const err: QueryError = {
      code,
      retriable: false,
      retryAfterMs: null,
      safeMessage,
      requestId: null,
    };
    this.options.onFatalError?.(err);
    this.options.onUiEnvelope?.({
      protocolVersion: PROTOCOL_VERSION,
      kind: "core.error",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload: err,
    });
  }

  private async spawnAndHandshake(hello: HostHelloPayload): Promise<CoreReadyPayload> {
    const binPath = resolveDaemonBinaryPath(this.options.daemonBin);
    if (!binPath) {
      this.status = "missing_binary";
      const msg =
        "ytm-daemon sidecar binary not found; refusing to fall back to JS business logic";
      this.log("error", "daemon_binary_missing", { explicit: this.options.daemonBin ?? null });
      this.emitFatalError("BRIDGE_INCOMPATIBLE", msg);
      throw new Error(msg);
    }

    this.status = "starting";
    this.coreEpoch = null;
    this.stdoutCarry = Buffer.alloc(0);
    this.discardingOversizedLine = false;
    this.discardedBytes = 0;

    const child = spawn(binPath, [], {
      cwd: this.options.cwd ?? process.cwd(),
      env: this.options.env ?? process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child = child;

    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      for (const line of chunk.split(/\r?\n/)) {
        const trimmed = line.trim();
        if (trimmed) {
          this.log("info", "daemon_stderr", { line: trimmed });
        }
      }
    });

    child.stdout.on("data", (chunk: Buffer) => {
      this.handleStdoutChunk(chunk);
    });

    child.on("error", (err) => {
      this.log("error", "daemon_spawn_error", { message: err.message });
    });

    child.on("exit", (code, signal) => {
      if (this.child !== child) {
        return;
      }
      this.child = null;
      this.handleProcessExit(code, signal);
    });

    const requestId = `hello-${randomUUID()}`;
    const readyPromise = new Promise<CoreReadyPayload>((resolveHandshake, rejectHandshake) => {
      const timer = setTimeout(() => {
        if (this.pendingHandshake?.requestId === requestId) {
          this.pendingHandshake = null;
          this.status = "incompatible";
          const msg = `ytm-daemon handshake timed out after ${this.handshakeTimeoutMs}ms`;
          this.emitFatalError("TIMEOUT", msg);
          rejectHandshake(new Error(msg));
        }
      }, this.handshakeTimeoutMs);

      this.pendingHandshake = {
        requestId,
        resolve: resolveHandshake,
        reject: rejectHandshake,
        timer,
      };
    });

    const helloEnvelope: Envelope = {
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.hello",
      coreEpoch: null,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId,
      seq: this.nextSeq(),
      payload: hello,
    };

    try {
      this.writeRawEnvelope(helloEnvelope);
    } catch (err) {
      if (this.pendingHandshake) {
        clearTimeout(this.pendingHandshake.timer);
        this.pendingHandshake = null;
      }
      throw err;
    }

    const ready = await readyPromise;
    if (ready.protocolVersion !== PROTOCOL_VERSION) {
      this.status = "incompatible";
      const msg = `Protocol version mismatch: expected ${PROTOCOL_VERSION}, got ${ready.protocolVersion}`;
      this.emitFatalError("BRIDGE_INCOMPATIBLE", msg);
      await this.stop();
      throw new Error(msg);
    }

    this.coreEpoch = ready.coreEpoch;
    this.status = "ready";
    this.flushPendingStartupFrames();
    // Host health updated while this handshake was in flight reached only the cached hello.
    if (this.lastHelloPayload && this.lastHelloPayload.hostHealth !== hello.hostHealth) {
      this.sendCachedHello();
    }
    return ready;
  }

  private handleStdoutChunk(chunk: Buffer): void {
    let offset = 0;
    while (offset < chunk.length) {
      const nlIndex = chunk.indexOf(0x0a, offset);
      if (nlIndex === -1) {
        const slice = chunk.subarray(offset);
        if (this.discardingOversizedLine) {
          this.discardedBytes += slice.length;
        } else if (this.stdoutCarry.length + slice.length > MAX_FRAME_BYTES + 1) {
          this.discardingOversizedLine = true;
          this.discardedBytes = this.stdoutCarry.length + slice.length;
          this.stdoutCarry = Buffer.alloc(0);
        } else {
          this.stdoutCarry = Buffer.concat([this.stdoutCarry, slice]);
        }
        break;
      }

      const slice = chunk.subarray(offset, nlIndex);
      offset = nlIndex + 1;

      if (this.discardingOversizedLine) {
        const total = this.discardedBytes + slice.length;
        this.discardingOversizedLine = false;
        this.discardedBytes = 0;
        this.stdoutCarry = Buffer.alloc(0);
        this.log("warn", "daemon_oversized_stdout_frame", { bytes: total });
        this.emitFatalError("INVALID_PAYLOAD", `Daemon emitted frame exceeding ${MAX_FRAME_BYTES} bytes`);
        continue;
      }

      const rawLine = this.stdoutCarry.length > 0 ? Buffer.concat([this.stdoutCarry, slice]) : slice;
      this.stdoutCarry = Buffer.alloc(0);

      const trimmed =
        rawLine.length > 0 && rawLine[rawLine.length - 1] === 0x0d
          ? rawLine.subarray(0, rawLine.length - 1)
          : rawLine;

      if (trimmed.length === 0) {
        continue;
      }
      if (trimmed.byteLength > MAX_FRAME_BYTES) {
        this.log("warn", "daemon_oversized_stdout_frame", { bytes: trimmed.byteLength });
        this.emitFatalError("INVALID_PAYLOAD", `Daemon emitted frame exceeding ${MAX_FRAME_BYTES} bytes`);
        continue;
      }

      this.handleStdoutLine(trimmed);
    }
  }

  private handleStdoutLine(line: Uint8Array): void {
    const validated = parseAndValidateEnvelope(line);
    if (!validated.ok) {
      this.log("warn", "daemon_invalid_stdout_envelope", {
        code: validated.code,
        message: validated.message,
      });
      if (validated.code === "BRIDGE_INCOMPATIBLE") {
        this.status = "incompatible";
        this.emitFatalError("BRIDGE_INCOMPATIBLE", validated.message);
        if (this.pendingHandshake) {
          clearTimeout(this.pendingHandshake.timer);
          const pending = this.pendingHandshake;
          this.pendingHandshake = null;
          pending.reject(new Error(validated.message));
        }
        void this.stop();
      }
      return;
    }

    const env = validated.value;

    // Only kinds the core role may emit are routed anywhere (never ui.*, host.* or bridge.*).
    if (!isKindAllowedForRole(env.kind, "core")) {
      this.log("warn", "daemon_forbidden_kind_dropped", { kind: env.kind });
      return;
    }

    if (env.kind === "core.ready") {
      const payload = env.payload as CoreReadyPayload;
      if (this.pendingHandshake) {
        clearTimeout(this.pendingHandshake.timer);
        const pending = this.pendingHandshake;
        this.pendingHandshake = null;
        if (payload.protocolVersion !== PROTOCOL_VERSION) {
          const msg = `Incompatible daemon protocolVersion ${payload.protocolVersion}`;
          this.status = "incompatible";
          this.emitFatalError("BRIDGE_INCOMPATIBLE", msg);
          pending.reject(new Error(msg));
          return;
        }
        pending.resolve(payload);
      }
      return;
    }

    if (env.kind === "core.pong") {
      const actionFromReq = env.requestId === "mpris:raise" || env.requestId === "mpris:quit"
        ? env.requestId
        : null;
      const payloadObj =
        env.payload !== null && typeof env.payload === "object"
          ? (env.payload as Record<string, unknown>)
          : null;
      const actionFromPayload =
        payloadObj && (payloadObj.action === "mpris:raise" || payloadObj.action === "mpris:quit")
          ? payloadObj.action
          : null;
      const mprisAction = actionFromReq ?? actionFromPayload;
      if (mprisAction) {
        this.options.onMprisAction?.(mprisAction);
        return;
      }
      this.options.onUiEnvelope?.(env);
      return;
    }

    if (env.kind === "core.outputGateControl") {
      const payload = env.payload as OutputGateControlPayload;
      void this.handleOutputGateControl(payload);
      return;
    }

    if (env.kind === "core.environmentConfig") {
      this.options.onEnvironmentConfig?.(env.payload as EnvironmentConfigPayload);
      return;
    }

    if (env.kind === "core.assetResolved") {
      const payload = env.payload as AssetResolvedPayload;
      if (env.requestId && this.pendingAssetRequests.has(env.requestId)) {
        const pending = this.pendingAssetRequests.get(env.requestId)!;
        clearTimeout(pending.timer);
        this.pendingAssetRequests.delete(env.requestId);
        pending.resolve(payload);
      }
      return;
    }

    if (env.kind === "player.command" || env.kind === "player.snapshotRequest") {
      this.options.onBridgeEnvelope?.(env);
      return;
    }

    if (env.kind === "core.error" && env.requestId?.startsWith(SESSION_REQUEST_PREFIX)) {
      // A host request: never shown to the UI, which learns auth state from core deltas.
      const qerr = env.payload as QueryError;
      const latest = env.requestId === this.latestSessionRequestId;
      this.log("warn", "session_material_rejected", { code: qerr.code, retriable: qerr.retriable, latest });
      if (latest) {
        this.options.onSessionMaterialError?.(qerr);
      }
      return;
    }

    if (
      env.kind === "core.error" &&
      this.pendingHandshake &&
      env.requestId === this.pendingHandshake.requestId
    ) {
      const qerr = env.payload as QueryError;
      clearTimeout(this.pendingHandshake.timer);
      const pending = this.pendingHandshake;
      this.pendingHandshake = null;
      if (qerr.code === "BRIDGE_INCOMPATIBLE") {
        this.status = "incompatible";
      }
      this.emitFatalError(qerr.code, qerr.safeMessage);
      pending.reject(new Error(qerr.safeMessage));
      return;
    }

    this.options.onUiEnvelope?.(env);
  }

  private async handleOutputGateControl(payload: OutputGateControlPayload): Promise<void> {
    let result: OutputGateResultPayload;
    if (this.options.onOutputGateControl) {
      try {
        result = await this.options.onOutputGateControl(payload);
      } catch (err) {
        result = {
          transactionId: payload.transactionId,
          applied: false,
          error: err instanceof Error ? err.message : String(err),
        };
      }
    } else {
      result = {
        transactionId: payload.transactionId,
        applied: false,
        error: "output gate handler is not registered on host",
      };
    }

    this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.outputGateResult",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload: result,
    });
  }

  private handleProcessExit(code: number | null, signal: NodeJS.Signals | null): void {
    if (this.pendingHandshake) {
      clearTimeout(this.pendingHandshake.timer);
      const pending = this.pendingHandshake;
      this.pendingHandshake = null;
      pending.reject(new Error(`ytm-daemon exited during handshake (code=${code}, signal=${signal})`));
    }

    for (const [rid, pending] of this.pendingAssetRequests.entries()) {
      clearTimeout(pending.timer);
      pending.reject(new Error("ytm-daemon exited before asset resolution completed"));
      this.pendingAssetRequests.delete(rid);
    }

    if (this.stopping || this.status === "incompatible") {
      if (this.stopping) {
        this.status = "stopped";
      }
      return;
    }

    const canRestart = this.restartCount < this.maxRestarts && this.lastHelloPayload !== null;
    this.status = "crashed";
    this.coreEpoch = null;
    // Drop any pre-crash startup frames so stale playback/write requests are never replayed.
    this.pendingStartupFrames = [];

    this.log("error", "daemon_unexpected_exit", {
      code,
      signal,
      restartCount: this.restartCount,
      willRestart: canRestart,
    });

    // Immediately notify host so player audio is muted/paused/closed with zero orphan playback.
    void Promise.resolve(
      this.options.onUnexpectedExit?.({
        code,
        signal,
        willRestart: canRestart,
        restartCount: this.restartCount,
      })
    );

    if (!canRestart) {
      this.emitFatalError(
        "BRIDGE_INCOMPATIBLE",
        `ytm-daemon exited unexpectedly (code=${code ?? "null"}, signal=${signal ?? "null"}); restart limit reached`
      );
      return;
    }

    this.restartCount += 1;
    const baseHello = this.lastHelloPayload!;
    const restartHello: HostHelloPayload = {
      ...baseHello,
      hostHealth: {
        ...baseHello.hostHealth,
        coreRestartCount: this.restartCount,
      },
    };
    this.lastHelloPayload = restartHello;

    void this.spawnAndHandshake(restartHello)
      .then(async (ready) => {
        this.options.onUiEnvelope?.({
          protocolVersion: PROTOCOL_VERSION,
          kind: "core.ready",
          coreEpoch: ready.coreEpoch,
          accountEpoch: null,
          bridgeEpoch: null,
          requestId: null,
          seq: this.nextSeq(),
          payload: ready,
        });
        await this.options.onControlledRestarted?.(ready);
      })
      .catch((err) => {
        this.status = "crashed";
        const msg = err instanceof Error ? err.message : String(err);
        this.emitFatalError("BRIDGE_INCOMPATIBLE", `Controlled restart failed: ${msg}`);
      });
  }

  private flushPendingStartupFrames(): void {
    const queued = this.pendingStartupFrames;
    this.pendingStartupFrames = [];
    for (const env of queued) {
      this.sendEnvelope(env);
    }
  }

  private writeRawEnvelope(env: Envelope): boolean {
    if (!this.child || !this.child.stdin.writable) {
      return false;
    }
    const line = serializeEnvelope(env);
    this.child.stdin.write(line);
    return true;
  }

  public sendEnvelope(envelope: Envelope): boolean {
    const checked = validateEnvelopeObject(envelope);
    if (!checked.ok) {
      this.log("warn", "host_refused_invalid_outbound_envelope", {
        kind: envelope.kind,
        code: checked.code,
        message: checked.message,
      });
      return false;
    }

    // Stamp unstamped frames at the host boundary, including tray commands. An
    // explicit previous epoch must reach the daemon unchanged so it can reject it.
    const enriched: Envelope = {
      ...checked.value,
      coreEpoch: checked.value.coreEpoch ?? this.coreEpoch,
    };

    if (this.status !== "ready" && enriched.kind !== "host.hello" && enriched.kind !== "host.shutdown") {
      if (this.status === "starting" && this.pendingStartupFrames.length < MAX_QUEUED_STARTUP_FRAMES) {
        this.pendingStartupFrames.push(enriched);
        return true;
      }
      return false;
    }

    return this.writeRawEnvelope(enriched);
  }

  public sendSessionMaterial(payload: SessionMaterialPayload): boolean {
    // NEVER log SessionMaterialPayload.
    const requestId = `${SESSION_REQUEST_PREFIX}${randomUUID()}`;
    this.latestSessionRequestId = requestId;
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.sessionMaterial",
      coreEpoch: this.coreEpoch,
      accountEpoch: payload.accountEpoch,
      bridgeEpoch: null,
      requestId,
      seq: this.nextSeq(),
      payload,
    });
  }

  public sendSessionCleared(): boolean {
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.sessionCleared",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload: {},
    });
  }

  public sendBridgeLifecycle(payload: BridgeLifecyclePayload): boolean {
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.bridgeLifecycle",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: payload.bridgeEpoch ?? null,
      requestId: null,
      seq: this.nextSeq(),
      payload,
    });
  }

  public sendWindowVisibility(payload: HostWindowVisibilityPayload): boolean {
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.windowVisibility",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload,
    });
  }

  /**
   * Re-announces host health that became known after the handshake (e.g. GPU feature status
   * once the GPU process reported). A repeated host.hello replaces the core's host health; its
   * core.ready reply is ignored outside a handshake. The cached hello always follows, so a
   * false return (core not ready) still reaches the core after its handshake or restart.
   */
  public updateHostHealth(patch: Partial<HostHelloPayload["hostHealth"]>): boolean {
    if (!this.lastHelloPayload) {
      return false;
    }
    this.lastHelloPayload = {
      ...this.lastHelloPayload,
      hostHealth: { ...this.lastHelloPayload.hostHealth, ...patch },
    };
    if (this.status !== "ready") {
      return false;
    }
    return this.sendCachedHello();
  }

  private sendCachedHello(): boolean {
    if (!this.lastHelloPayload) {
      return false;
    }
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.hello",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: `hello-update-${randomUUID()}`,
      seq: this.nextSeq(),
      payload: this.lastHelloPayload,
    });
  }

  /**
   * Reports a connectivity change. The cached hello follows it, so a controlled restart
   * hands the new core the current state rather than the one seen at startup.
   */
  public sendNetworkChanged(network: NetworkState): boolean {
    if (this.lastHelloPayload) {
      this.lastHelloPayload = { ...this.lastHelloPayload, networkState: network };
    }
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.networkChanged",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload: network,
    });
  }

  public sendPowerResume(): boolean {
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.powerResume",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload: {},
    });
  }

  public sendEnvironmentSnapshot(payload: HostEnvironmentSnapshotPayload): boolean {
    return this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "host.environmentSnapshot",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: null,
      seq: this.nextSeq(),
      payload,
    });
  }

  public async resolveAsset(assetId: string, timeoutMs = 5_000): Promise<AssetResolvedPayload> {
    if (this.status !== "ready" || !this.child) {
      throw new Error("ytm-daemon is not ready for asset resolution");
    }
    const requestId = `asset-${randomUUID()}`;
    const promise = new Promise<AssetResolvedPayload>((resolveAsset, rejectAsset) => {
      const timer = setTimeout(() => {
        this.pendingAssetRequests.delete(requestId);
        rejectAsset(new Error(`Timed out resolving asset ${assetId}`));
      }, timeoutMs);
      this.pendingAssetRequests.set(requestId, {
        resolve: resolveAsset,
        reject: rejectAsset,
        timer,
      });
    });

    const sent = this.sendEnvelope({
      protocolVersion: PROTOCOL_VERSION,
      kind: "core.resolveAsset",
      coreEpoch: this.coreEpoch,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId,
      seq: this.nextSeq(),
      payload: { assetId },
    });
    if (!sent) {
      const pending = this.pendingAssetRequests.get(requestId);
      if (pending) {
        clearTimeout(pending.timer);
        this.pendingAssetRequests.delete(requestId);
      }
      throw new Error("Failed to send core.resolveAsset to ytm-daemon");
    }

    return promise;
  }

  public async stop(): Promise<void> {
    this.stopping = true;
    const child = this.child;
    if (!child?.pid) {
      this.child = null;
      this.status = "stopped";
      return;
    }

    try {
      this.writeRawEnvelope({
        protocolVersion: PROTOCOL_VERSION,
        kind: "host.shutdown",
        coreEpoch: this.coreEpoch,
        accountEpoch: null,
        bridgeEpoch: null,
        requestId: null,
        seq: this.nextSeq(),
        payload: {},
      });
    } catch {
      // Ignore write error during shutdown
    }

    try {
      child.stdin.end();
    } catch {
      // Ignore stdin close error
    }

    await new Promise<void>((resolveStop) => {
      let killTimer: NodeJS.Timeout | undefined;
      const termTimer = setTimeout(() => {
        child.kill("SIGTERM");
        killTimer = setTimeout(() => child.kill("SIGKILL"), 300);
      }, 1_500);

      child.once("exit", () => {
        clearTimeout(termTimer);
        clearTimeout(killTimer);
        resolveStop();
      });
    });

    this.child = null;
    this.status = "stopped";
  }
}
