import {
  HOST_SECRET_KINDS,
  MAX_FRAME_BYTES,
  isKindAllowedForRole,
  parseAndValidateEnvelope,
  validateEnvelopeObject,
  type Envelope,
  type QueryErrorCode,
  type SenderRole,
} from "@ytm-glass/protocol";
import type { IpcMainEvent, IpcMainInvokeEvent, WebContents, WebFrameMain } from "electron";

export type RendererRole = Extract<SenderRole, "mainUi" | "playerBridge">;

// Protocol envelopes are shallow. Bound traversal of structured-clone data
// before serialization and schema validation in the main process.
const MAX_IPC_OBJECT_DEPTH = 64;

export interface IpcFrameDescriptor {
  readonly isMainFrame: boolean;
  readonly origin: string;
  readonly url?: string;
}

export interface IpcValidationContext {
  readonly role: RendererRole;
  readonly allowedOrigins: ReadonlySet<string>;
  readonly senderMatchesExpected: boolean;
  readonly senderFrame: IpcFrameDescriptor | null;
  readonly bridgeOriginActive?: boolean;
}

export type IpcGuardResult =
  | {
      readonly ok: true;
      readonly envelope: Envelope;
      readonly role: RendererRole;
      readonly origin: string;
    }
  | {
      readonly ok: false;
      readonly code: QueryErrorCode;
      readonly message: string;
      readonly reason:
        | "destroyed_or_missing_target"
        | "unexpected_web_contents"
        | "missing_sender_frame"
        | "subframe_rejected"
        | "disallowed_origin"
        | "bridge_inactive_for_origin"
        | "payload_too_large"
        | "invalid_payload"
        | "role_forbidden";
    };

/**
 * Estimates serialized UTF-8 byte size with an immediate abort threshold so
 * adversarial or oversized IPC objects are rejected before schema validation.
 */
function measureJsonByteBudget(
  value: unknown,
  budget: number,
  seen: Set<object>,
  depth: number
): { ok: true; bytes: number } | { ok: false; reason: "oversize" | "unserializable" } {
  if (value === null) {
    return { ok: true, bytes: 4 };
  }
  const t = typeof value;
  if (t === "boolean") {
    return { ok: true, bytes: value ? 4 : 5 };
  }
  if (t === "number") {
    if (!Number.isFinite(value)) {
      return { ok: false, reason: "unserializable" };
    }
    return { ok: true, bytes: String(value).length };
  }
  if (t === "string") {
    // Worst-case JSON overhead (quotes + potential escaping) bounded safely via UTF-8 byte length.
    const bytes = Buffer.byteLength(value as string, "utf8") + 2;
    return bytes > budget ? { ok: false, reason: "oversize" } : { ok: true, bytes };
  }
  if (t === "undefined") {
    return { ok: true, bytes: 4 };
  }
  if (t !== "object") {
    return { ok: false, reason: "unserializable" };
  }

  const obj = value as object;
  if (depth === 0 || seen.has(obj)) {
    return { ok: false, reason: "unserializable" };
  }
  seen.add(obj);

  let total = 2;
  if (Array.isArray(obj)) {
    for (let i = 0; i < obj.length; i++) {
      if (i > 0) total += 1;
      if (total > budget) {
        seen.delete(obj);
        return { ok: false, reason: "oversize" };
      }
      const child = measureJsonByteBudget(obj[i], budget - total, seen, depth - 1);
      if (!child.ok) {
        seen.delete(obj);
        return child;
      }
      total += child.bytes;
      if (total > budget) {
        seen.delete(obj);
        return { ok: false, reason: "oversize" };
      }
    }
    seen.delete(obj);
    return { ok: true, bytes: total };
  }

  const entries = Object.entries(obj as Record<string, unknown>);
  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    if (!entry) continue;
    const [k, v] = entry;
    if (v === undefined) continue;
    if (i > 0) total += 1;
    total += Buffer.byteLength(k, "utf8") + 3;
    if (total > budget) {
      seen.delete(obj);
      return { ok: false, reason: "oversize" };
    }
    const child = measureJsonByteBudget(v, budget - total, seen, depth - 1);
    if (!child.ok) {
      seen.delete(obj);
      return child;
    }
    total += child.bytes;
    if (total > budget) {
      seen.delete(obj);
      return { ok: false, reason: "oversize" };
    }
  }

  seen.delete(obj);
  return { ok: true, bytes: total };
}

/**
 * Pure IPC security validator enforcing sender identity, top-level frame,
 * origin allowlist, 1 MiB byte budget, schema validation, and role allowlist.
 */
export function validateRendererIpc(
  context: IpcValidationContext,
  rawPayload: unknown
): IpcGuardResult {
  if (!context.senderMatchesExpected) {
    return {
      ok: false,
      code: "STALE_CONTEXT",
      message: `IPC sender does not match expected ${context.role} webContents`,
      reason: "unexpected_web_contents",
    };
  }

  if (!context.senderFrame) {
    return {
      ok: false,
      code: "STALE_CONTEXT",
      message: "IPC senderFrame is missing",
      reason: "missing_sender_frame",
    };
  }

  if (!context.senderFrame.isMainFrame) {
    return {
      ok: false,
      code: "STALE_CONTEXT",
      message: "IPC rejected from subframe",
      reason: "subframe_rejected",
    };
  }

  const origin = context.senderFrame.origin;
  if (!origin || origin === "null" || !context.allowedOrigins.has(origin)) {
    return {
      ok: false,
      code: "STALE_CONTEXT",
      message: `IPC rejected from disallowed origin for ${context.role}`,
      reason: "disallowed_origin",
    };
  }

  if (context.role === "playerBridge" && context.bridgeOriginActive === false) {
    return {
      ok: false,
      code: "STALE_CONTEXT",
      message: "Bridge IPC disabled because player is not on music.youtube.com",
      reason: "bridge_inactive_for_origin",
    };
  }

  let validatedEnvelope: Envelope;
  if (typeof rawPayload === "string" || rawPayload instanceof Uint8Array) {
    const rawBytes =
      typeof rawPayload === "string"
        ? Buffer.byteLength(rawPayload, "utf8")
        : rawPayload.byteLength;
    if (rawBytes > MAX_FRAME_BYTES) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `IPC payload exceeds ${MAX_FRAME_BYTES} byte limit`,
        reason: "payload_too_large",
      };
    }
    const parsed = parseAndValidateEnvelope(rawPayload);
    if (!parsed.ok) {
      return {
        ok: false,
        code: parsed.code as QueryErrorCode,
        message: parsed.message,
        reason: "invalid_payload",
      };
    }
    validatedEnvelope = parsed.value;
  } else {
    const budgetCheck = measureJsonByteBudget(rawPayload, MAX_FRAME_BYTES, new Set(), MAX_IPC_OBJECT_DEPTH);
    if (!budgetCheck.ok) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message:
          budgetCheck.reason === "oversize"
            ? `IPC payload exceeds ${MAX_FRAME_BYTES} byte limit`
            : "IPC payload is not JSON-serializable",
        reason: budgetCheck.reason === "oversize" ? "payload_too_large" : "invalid_payload",
      };
    }

    let serializedBytes: number;
    try {
      serializedBytes = Buffer.byteLength(JSON.stringify(rawPayload), "utf8");
    } catch {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: "IPC payload serialization failed",
        reason: "invalid_payload",
      };
    }
    if (serializedBytes > MAX_FRAME_BYTES) {
      return {
        ok: false,
        code: "INVALID_PAYLOAD",
        message: `IPC payload exceeds ${MAX_FRAME_BYTES} byte limit`,
        reason: "payload_too_large",
      };
    }

    const parsed = validateEnvelopeObject(rawPayload);
    if (!parsed.ok) {
      return {
        ok: false,
        code: parsed.code as QueryErrorCode,
        message: parsed.message,
        reason: "invalid_payload",
      };
    }
    validatedEnvelope = parsed.value;
  }

  if (HOST_SECRET_KINDS.has(validatedEnvelope.kind)) {
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `Secret message kind ${validatedEnvelope.kind} is forbidden on renderer IPC`,
      reason: "role_forbidden",
    };
  }

  if (!isKindAllowedForRole(validatedEnvelope.kind, context.role)) {
    return {
      ok: false,
      code: "INVALID_PAYLOAD",
      message: `Message kind ${validatedEnvelope.kind} is not allowed for role ${context.role}`,
      reason: "role_forbidden",
    };
  }

  return {
    ok: true,
    envelope: validatedEnvelope,
    role: context.role,
    origin,
  };
}

export interface ExpectedSenderTarget {
  readonly role: RendererRole;
  readonly allowedOrigins: ReadonlySet<string>;
  readonly getWebContents: () => WebContents | null;
  readonly isBridgeOriginActive?: () => boolean;
}

/**
 * Validates a live Electron IPC event against the expected WebContents, its
 * top-level `mainFrame`, its actual `senderFrame.origin`, 1 MiB budget,
 * protocol schema, and role allowlist.
 */
export function validateElectronIpcEvent(
  event: Pick<IpcMainEvent | IpcMainInvokeEvent, "sender" | "senderFrame">,
  target: ExpectedSenderTarget,
  rawPayload: unknown
): IpcGuardResult {
  const expectedWebContents = target.getWebContents();
  if (!expectedWebContents || expectedWebContents.isDestroyed()) {
    return {
      ok: false,
      code: "STALE_CONTEXT",
      message: `Expected ${target.role} WebContents is unavailable`,
      reason: "destroyed_or_missing_target",
    };
  }

  const expectedMainFrame: WebFrameMain | null = expectedWebContents.mainFrame ?? null;
  const senderFrame = event.senderFrame ?? null;

  const senderFrameDescriptor: IpcFrameDescriptor | null = senderFrame
    ? {
        isMainFrame:
          expectedMainFrame !== null &&
          senderFrame === expectedMainFrame &&
          senderFrame.parent === null,
        origin: senderFrame.origin,
        url: senderFrame.url,
      }
    : null;

  return validateRendererIpc(
    {
      role: target.role,
      allowedOrigins: target.allowedOrigins,
      senderMatchesExpected: event.sender === expectedWebContents,
      senderFrame: senderFrameDescriptor,
      bridgeOriginActive: target.isBridgeOriginActive ? target.isBridgeOriginActive() : undefined,
    },
    rawPayload
  );
}
