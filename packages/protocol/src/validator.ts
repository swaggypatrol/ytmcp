import Ajv, { type ValidateFunction } from "ajv";
import addFormats from "ajv-formats";
import schemaJson from "../schema/protocol.v1.schema.json" with { type: "json" };
import type { Envelope, MessageKind } from "./generated/types.js";

export const PROTOCOL_VERSION = 1;
export const MAX_FRAME_BYTES = 1_048_576; // 1 MiB
export const MAX_PENDING_REQUESTS = 128;
export const MAX_COMMAND_QUEUE = 64;
export const MAX_PAGE_SIZE = 100;
export const MAX_SAFE_JS_INT = Number.MAX_SAFE_INTEGER;

export type SenderRole = "mainUi" | "playerBridge" | "host" | "core";

export const MAIN_UI_ALLOWED_OUTBOUND: ReadonlySet<MessageKind> = new Set<MessageKind>([
  "ui.subscribeSnapshot",
  "ui.unsubscribe",
  "ui.playerCommand",
  "ui.catalogQuery",
  "ui.lyricsQuery",
  "ui.mutation",
  "ui.agentActivity",
  "ui.settingsUpdate",
  "ui.momentSettings",
  "ui.openPlayerWindow",
  "ui.logout",
  "ui.clearCache",
  "ui.ping",
]);

export const BRIDGE_ALLOWED_OUTBOUND: ReadonlySet<MessageKind> = new Set<MessageKind>([
  "bridge.ready",
  "bridge.playbackObserved",
  "bridge.queueObserved",
  "bridge.health",
  "bridge.commandAck",
  "bridge.error",
]);

export const HOST_SECRET_KINDS: ReadonlySet<MessageKind> = new Set<MessageKind>([
  "host.sessionMaterial",
]);

export type ValidationResult<T> =
  | { ok: true; value: T }
  | { ok: false; code: string; message: string };

let compiledValidators: {
  validateEnvelopeSchema: ValidateFunction;
  payloadValidators: Partial<Record<MessageKind, ValidateFunction>>;
} | null = null;

function ensureValidators(): {
  validateEnvelopeSchema: ValidateFunction;
  payloadValidators: Partial<Record<MessageKind, ValidateFunction>>;
} {
  if (compiledValidators) {
    return compiledValidators;
  }
  const ajv = new Ajv({
    allErrors: false,
    strict: false,
    allowUnionTypes: true,
  });
  addFormats(ajv);
  for (const fmt of ["uint8", "uint16", "uint32", "uint64", "int32", "int64", "double", "float"]) {
    ajv.addFormat(fmt, { type: "number", validate: () => true });
  }
  ajv.addSchema(schemaJson, "protocol-root");

  const compileRef = (ref: string): ValidateFunction => {
    const fn = ajv.getSchema(`protocol-root${ref}`);
    if (!fn) {
      throw new Error(`Missing schema definition for ${ref}`);
    }
    return fn;
  };

  compiledValidators = {
    validateEnvelopeSchema: compileRef("#/definitions/Envelope"),
    payloadValidators: {
      "ui.playerCommand": compileRef("#/definitions/PlayerCommandPayload"),
      "player.command": compileRef("#/definitions/PlayerCommandPayload"),
      "ui.catalogQuery": compileRef("#/definitions/CatalogQueryRequest"),
      "ui.lyricsQuery": compileRef("#/definitions/LyricsQueryRequest"),
      "ui.mutation": compileRef("#/definitions/MutationRequest"),
      "ui.agentActivity": compileRef("#/definitions/AgentActivityRequest"),
      "ui.settingsUpdate": compileRef("#/definitions/AppSettingsPatch"),
      "ui.momentSettings": compileRef("#/definitions/MomentDesktopRequest"),
      "ui.openPlayerWindow": compileRef("#/definitions/OpenPlayerWindowPayload"),
      "core.snapshot": compileRef("#/definitions/AppSnapshot"),
      "core.delta": compileRef("#/definitions/StateDelta"),
      "core.commandReceipt": compileRef("#/definitions/CommandReceipt"),
      "core.catalogResponse": compileRef("#/definitions/CatalogResponsePayload"),
      "core.lyricsResponse": compileRef("#/definitions/LyricsResult"),
      "core.mutationResponse": compileRef("#/definitions/MutationResult"),
      "core.agentActivity": compileRef("#/definitions/AgentActivitySnapshot"),
      "core.settingsUpdated": compileRef("#/definitions/AppSettings"),
      "core.momentSettings": compileRef("#/definitions/MomentDesktopSnapshot"),
      "core.environmentConfig": compileRef("#/definitions/EnvironmentConfigPayload"),
      "core.error": compileRef("#/definitions/QueryError"),
      "bridge.error": compileRef("#/definitions/QueryError"),
      "host.hello": compileRef("#/definitions/HostHelloPayload"),
      "core.ready": compileRef("#/definitions/CoreReadyPayload"),
      "host.windowVisibility": compileRef("#/definitions/HostWindowVisibilityPayload"),
      "host.networkChanged": compileRef("#/definitions/NetworkState"),
      "host.sessionMaterial": compileRef("#/definitions/SessionMaterialPayload"),
      "host.bridgeLifecycle": compileRef("#/definitions/BridgeLifecyclePayload"),
      "host.environmentSnapshot": compileRef("#/definitions/HostEnvironmentSnapshotPayload"),
      "core.outputGateControl": compileRef("#/definitions/OutputGateControlPayload"),
      "host.outputGateResult": compileRef("#/definitions/OutputGateResultPayload"),
      "core.resolveAsset": compileRef("#/definitions/ResolveAssetPayload"),
      "core.assetResolved": compileRef("#/definitions/AssetResolvedPayload"),
      "bridge.ready": compileRef("#/definitions/BridgeReadyPayload"),
      "bridge.playbackObserved": compileRef("#/definitions/BridgePlaybackObservedPayload"),
      "bridge.queueObserved": compileRef("#/definitions/QueueSnapshot"),
      "bridge.health": compileRef("#/definitions/BridgeHealthPayload"),
      "bridge.commandAck": compileRef("#/definitions/BridgeCommandAckPayload"),
    },
  };
  return compiledValidators;
}

const EMPTY_OBJECT_KINDS: ReadonlySet<MessageKind> = new Set<MessageKind>([
  "ui.subscribeSnapshot",
  "ui.unsubscribe",
  "ui.logout",
  "ui.clearCache",
  "ui.ping",
  "core.pong",
  "host.powerResume",
  "host.sessionCleared",
  "host.shutdown",
  "player.snapshotRequest",
]);

function checkNumericBounds(value: unknown): string | null {
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      return "non-finite number";
    }
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
      return `integer ${value} outside safe JS range`;
    }
    return null;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      const err = checkNumericBounds(item);
      if (err) return err;
    }
    return null;
  }
  if (value !== null && typeof value === "object") {
    for (const v of Object.values(value)) {
      const err = checkNumericBounds(v);
      if (err) return err;
    }
  }
  return null;
}

export function isKindAllowedForRole(kind: MessageKind, role: SenderRole): boolean {
  switch (role) {
    case "mainUi":
      return MAIN_UI_ALLOWED_OUTBOUND.has(kind);
    case "playerBridge":
      return BRIDGE_ALLOWED_OUTBOUND.has(kind);
    case "host":
      return (
        kind === "host.hello" ||
        kind === "host.windowVisibility" ||
        kind === "host.powerResume" ||
        kind === "host.networkChanged" ||
        kind === "host.sessionMaterial" ||
        kind === "host.sessionCleared" ||
        kind === "host.bridgeLifecycle" ||
        kind === "host.outputGateResult" ||
        kind === "host.environmentSnapshot" ||
        kind === "core.resolveAsset" ||
        kind === "host.shutdown"
      );
    case "core":
      return (
        kind === "core.ready" ||
        kind === "core.snapshot" ||
        kind === "core.delta" ||
        kind === "core.commandReceipt" ||
        kind === "core.catalogResponse" ||
        kind === "core.lyricsResponse" ||
        kind === "core.mutationResponse" ||
        kind === "core.agentActivity" ||
        kind === "core.settingsUpdated" ||
        kind === "core.momentSettings" ||
        kind === "core.environmentConfig" ||
        kind === "core.error" ||
        kind === "core.pong" ||
        kind === "core.outputGateControl" ||
        kind === "core.assetResolved" ||
        kind === "player.command" ||
        kind === "player.snapshotRequest"
      );
  }
}

export function validatePayloadByKind(
  kind: MessageKind,
  payload: unknown
): ValidationResult<unknown> {
  if (EMPTY_OBJECT_KINDS.has(kind)) {
    if (
      payload !== null &&
      (typeof payload !== "object" || Array.isArray(payload))
    ) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `${kind} expects object or null payload`,
      };
    }
    return { ok: true, value: payload };
  }

  const validator = ensureValidators().payloadValidators[kind];
  if (!validator) {
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `no payload validator registered for ${kind}`,
    };
  }
  if (!validator(payload)) {
    const first = validator.errors?.[0];
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `${kind}: ${first?.instancePath ?? ""} ${first?.message ?? "schema mismatch"}`.trim(),
    };
  }

  if (kind === "ui.playerCommand" || kind === "player.command") {
    const p = payload as { args?: { volume?: number | null } | null };
    const vol = p.args?.volume;
    if (typeof vol === "number" && (vol < 0 || vol > 1)) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `volume ${vol} outside [0.0, 1.0]`,
      };
    }
  } else if (kind === "bridge.playbackObserved") {
    const obs = payload as { volume: number; playbackRate: number };
    if (obs.volume < 0 || obs.volume > 1) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `volume ${obs.volume} outside [0.0, 1.0]`,
      };
    }
    if (obs.playbackRate <= 0 || obs.playbackRate > 16) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `playbackRate ${obs.playbackRate} out of range`,
      };
    }
  } else if (kind === "ui.catalogQuery") {
    const q = payload as { limit?: number | null };
    if (typeof q.limit === "number" && (q.limit < 1 || q.limit > MAX_PAGE_SIZE)) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `limit ${q.limit} outside [1, ${MAX_PAGE_SIZE}]`,
      };
    }
  } else if (kind === "ui.settingsUpdate") {
    const patch = payload as { uiScalePercent?: number | null };
    if (
      typeof patch.uiScalePercent === "number" &&
      (patch.uiScalePercent < 75 || patch.uiScalePercent > 150)
    ) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `uiScalePercent ${patch.uiScalePercent} outside [75, 150]`,
      };
    }
  }

  return { ok: true, value: payload };
}

export function validateEnvelopeObject(raw: unknown): ValidationResult<Envelope> {
  const numErr = checkNumericBounds(raw);
  if (numErr) {
    return { ok: false, code: "INVALID_PAYLOAD", message: numErr };
  }

  if (
    raw !== null &&
    typeof raw === "object" &&
    "protocolVersion" in raw &&
    typeof (raw as { protocolVersion: unknown }).protocolVersion === "number" &&
    (raw as { protocolVersion: number }).protocolVersion !== PROTOCOL_VERSION
  ) {
    return {
      ok: false,
      code: "BRIDGE_INCOMPATIBLE",
      message: `unsupported protocolVersion ${(raw as { protocolVersion: number }).protocolVersion}`,
    };
  }

  const { validateEnvelopeSchema } = ensureValidators();
  if (!validateEnvelopeSchema(raw)) {
    const first = validateEnvelopeSchema.errors?.[0];
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `envelope: ${first?.instancePath ?? ""} ${first?.message ?? "schema mismatch"}`.trim(),
    };
  }

  const env = raw as unknown as Envelope;
  const payloadRes = validatePayloadByKind(env.kind, env.payload);
  if (!payloadRes.ok) {
    return payloadRes;
  }
  return { ok: true, value: env };
}

export function parseAndValidateEnvelope(
  rawLine: string | Uint8Array
): ValidationResult<Envelope> {
  const bytes =
    typeof rawLine === "string" ? Buffer.from(rawLine, "utf8") : Buffer.from(rawLine);
  let trimmed = bytes;
  if (trimmed.length > 0 && trimmed[trimmed.length - 1] === 0x0a) {
    trimmed = trimmed.subarray(0, trimmed.length - 1);
  }
  if (trimmed.length > 0 && trimmed[trimmed.length - 1] === 0x0d) {
    trimmed = trimmed.subarray(0, trimmed.length - 1);
  }

  if (trimmed.byteLength > MAX_FRAME_BYTES) {
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `frame exceeds ${MAX_FRAME_BYTES} bytes (${trimmed.byteLength})`,
    };
  }
  if (trimmed.includes(0x0a) || trimmed.includes(0x0d)) {
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: "frame contains raw newline",
    };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed.toString("utf8"));
  } catch (err) {
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `invalid JSON: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
  return validateEnvelopeObject(parsed);
}

export function serializeEnvelope(env: Envelope): string {
  const checked = validateEnvelopeObject(env);
  if (!checked.ok) {
    throw new Error(`Refusing to serialize invalid envelope (${checked.code}): ${checked.message}`);
  }
  const json = JSON.stringify(checked.value);
  if (Buffer.byteLength(json, "utf8") > MAX_FRAME_BYTES) {
    throw new Error(`Serialized frame exceeds ${MAX_FRAME_BYTES} bytes`);
  }
  return `${json}\n`;
}
