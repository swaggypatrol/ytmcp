import type {
  BridgeEpoch,
  Envelope,
  PlayerCommandPayload,
  PlayerOp,
  QueryError,
  QueryErrorCode,
  RepeatMode,
  Track,
} from "@ytm-glass/protocol";
import { isPlaylistEntryId, MUSIC_ORIGIN } from "./types.js";

export const PROTOCOL_VERSION = 1;

const VALID_PLAYER_OPS: ReadonlySet<string> = new Set<PlayerOp>([
  "play",
  "pause",
  "toggle",
  "stop",
  "seek",
  "setVolume",
  "setMuted",
  "next",
  "previous",
  "playTarget",
  "playCollection",
  "setShuffle",
  "setRepeat",
  "setAutoplay",
  "enqueue",
  "selectQueueEntry",
  "removeQueueEntry",
  "moveQueueEntry",
  "clearUpcoming",
]);

const VALID_REPEAT_MODES: ReadonlySet<string> = new Set<RepeatMode>(["off", "all", "one"]);

export const SAFE_VIDEO_ID_RE = /^[A-Za-z0-9_-]{1,64}$/;
export const SAFE_PLAYLIST_ID_RE = /^[A-Za-z0-9_-]{1,128}$/;

export function isPlainObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function isSafeNonNegInt(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Converts a page volume to the protocol's [0.0, 1.0]; `percent` is the YouTube player API scale. */
export function normalizeVolume(rawVolume: number, scale: "unit" | "percent"): number {
  if (!Number.isFinite(rawVolume)) return 1.0;
  const unit = scale === "percent" ? rawVolume / 100 : rawVolume;
  if (unit <= 0) return 0.0;
  if (unit >= 1) return 1.0;
  return Math.round(unit * 10_000) / 10_000;
}

export function normalizePlaybackRate(rawRate: number | null | undefined): number {
  return typeof rawRate === "number" && Number.isFinite(rawRate) && rawRate > 0 && rawRate <= 16 ? rawRate : 1.0;
}

export function toUint64Ms(rawMs: number | null | undefined): number | null {
  if (rawMs === null || rawMs === undefined || !Number.isFinite(rawMs) || rawMs < 0) return null;
  const rounded = Math.round(rawMs);
  return Number.isSafeInteger(rounded) ? rounded : null;
}

export function buildQueryError(code: QueryErrorCode, safeMessage: string, requestId: string | null = null): QueryError {
  return { code, retriable: false, safeMessage, requestId, retryAfterMs: null };
}

export function sanitizeHttpsUrl(rawUrl: string | null | undefined): string | null {
  if (!rawUrl || typeof rawUrl !== "string") return null;
  try {
    const parsed = new URL(rawUrl, MUSIC_ORIGIN);
    return parsed.protocol === "https:" ? parsed.toString() : null;
  } catch {
    return null;
  }
}

export function sanitizeTrack(input: {
  videoId: string;
  title: string;
  artists?: readonly string[];
  albumTitle?: string | null;
  durationMs?: number | null;
  coverUrl?: string | null;
  availability?: Track["availability"];
}): Track {
  const trimmedTitle = input.title.trim();
  const trimmedAlbum = input.albumTitle?.trim() ?? "";
  const coverUrl = sanitizeHttpsUrl(input.coverUrl);
  return {
    videoId: SAFE_VIDEO_ID_RE.test(input.videoId) ? input.videoId : "unknown",
    title:
      trimmedTitle.length > 0
        ? trimmedTitle
        : input.availability === "unavailable"
          ? "Unavailable track"
          : "Unknown track",
    artists: (input.artists ?? [])
      .map((name) => name.trim())
      .filter((name) => name.length > 0)
      .map((name) => ({ id: null, name })),
    album: trimmedAlbum.length > 0 ? { id: null, browseId: null, title: trimmedAlbum } : null,
    durationMs: toUint64Ms(input.durationMs),
    cover: coverUrl ? { assetId: null, accentOklch: null, remoteUrl: coverUrl } : null,
    availability: input.availability ?? "available",
    versionTag: null,
    isLiked: null,
  };
}

export type InboundValidationResult =
  | { readonly ok: true; readonly kind: "player.command"; readonly envelope: Envelope; readonly command: PlayerCommandPayload }
  | { readonly ok: true; readonly kind: "player.snapshotRequest"; readonly envelope: Envelope }
  | { readonly ok: false; readonly code: QueryErrorCode; readonly message: string; readonly requestId: string | null };

/** Validates host -> bridge envelopes: version, seq, epoch, kind and command argument shapes. */
export function validateInboundBridgeEnvelope(raw: unknown, expectedBridgeEpoch: BridgeEpoch): InboundValidationResult {
  if (!isPlainObject(raw)) {
    return { ok: false, code: "INVALID_PAYLOAD", message: "Bridge inbound envelope must be an object", requestId: null };
  }
  const requestId = typeof raw.requestId === "string" ? raw.requestId : null;
  const fail = (code: QueryErrorCode, message: string): InboundValidationResult => ({ ok: false, code, message, requestId });

  if (raw.protocolVersion !== PROTOCOL_VERSION) {
    return fail("BRIDGE_INCOMPATIBLE", `Unsupported protocolVersion ${String(raw.protocolVersion)}`);
  }
  if (!isSafeNonNegInt(raw.seq)) {
    return fail("INVALID_PAYLOAD", "Envelope seq must be a safe non-negative integer");
  }
  if (raw.bridgeEpoch !== undefined && raw.bridgeEpoch !== null && raw.bridgeEpoch !== expectedBridgeEpoch) {
    return fail("STALE_CONTEXT", "Envelope bridgeEpoch mismatch");
  }
  if (raw.kind === "player.snapshotRequest") {
    if (raw.payload !== null && raw.payload !== undefined && !isPlainObject(raw.payload)) {
      return fail("INVALID_PAYLOAD", "player.snapshotRequest payload must be object or null");
    }
    return { ok: true, kind: "player.snapshotRequest", envelope: raw as unknown as Envelope };
  }
  if (raw.kind !== "player.command") {
    return fail("INVALID_PAYLOAD", `Unexpected inbound kind for bridge: ${String(raw.kind)}`);
  }
  const payload = raw.payload;
  if (!isPlainObject(payload)) {
    return fail("INVALID_PAYLOAD", "player.command payload must be an object");
  }
  if (typeof payload.commandId !== "string" || payload.commandId.trim() === "") {
    return fail("INVALID_PAYLOAD", "player.command requires non-empty commandId");
  }
  if (typeof payload.op !== "string" || !VALID_PLAYER_OPS.has(payload.op)) {
    return fail("INVALID_PAYLOAD", `Unsupported player op: ${String(payload.op)}`);
  }
  if (payload.args !== undefined && payload.args !== null) {
    if (!isPlainObject(payload.args)) {
      return fail("INVALID_PAYLOAD", "player.command args must be an object or null");
    }
    const argsErr = validateCommandArgs(payload.args);
    if (argsErr) return fail("INVALID_PAYLOAD", argsErr);
  }
  return {
    ok: true,
    kind: "player.command",
    envelope: raw as unknown as Envelope,
    command: payload as unknown as PlayerCommandPayload,
  };
}

function validateCommandArgs(args: Record<string, unknown>): string | null {
  const present = (v: unknown): boolean => v !== undefined && v !== null;
  if (present(args.volume) && (typeof args.volume !== "number" || !Number.isFinite(args.volume) || args.volume < 0 || args.volume > 1)) {
    return "volume must be a finite number in [0.0, 1.0]";
  }
  if (present(args.muted) && typeof args.muted !== "boolean") return "muted must be a boolean";
  if (present(args.positionMs) && !isSafeNonNegInt(args.positionMs)) return "positionMs must be a safe non-negative integer";
  if (present(args.queueRevision) && !isSafeNonNegInt(args.queueRevision)) return "queueRevision must be a safe non-negative integer";
  for (const field of ["queueEntryId", "beforeQueueEntryId"]) {
    const value = args[field];
    if (present(value) && (typeof value !== "string" || value.trim().length === 0 || value.length > 256)) {
      return `${field} must be a valid occurrence identifier`;
    }
  }
  if (present(args.shuffle) && typeof args.shuffle !== "boolean") return "shuffle must be a boolean";
  if (present(args.autoplay) && typeof args.autoplay !== "boolean") return "autoplay must be a boolean";
  if (present(args.repeat) && (typeof args.repeat !== "string" || !VALID_REPEAT_MODES.has(args.repeat))) {
    return "repeat must be off | all | one";
  }
  if (present(args.enqueuePosition) && args.enqueuePosition !== "next" && args.enqueuePosition !== "last") {
    return "enqueuePosition must be next | last";
  }
  if (present(args.target)) {
    const target = args.target;
    if (!isPlainObject(target) || typeof target.videoId !== "string" || !SAFE_VIDEO_ID_RE.test(target.videoId)) {
      return "target.videoId must be a valid video identifier";
    }
    if (present(target.playlistId) && (typeof target.playlistId !== "string" || !SAFE_PLAYLIST_ID_RE.test(target.playlistId))) {
      return "target.playlistId must be a valid playlist identifier";
    }
    if (present(target.playlistEntryId) && !isPlaylistEntryId(target.playlistEntryId)) {
      return "target.playlistEntryId must be a valid playlist occurrence identifier";
    }
    if (present(target.startIndex) && !isSafeNonNegInt(target.startIndex)) {
      return "target.startIndex must be a safe non-negative integer";
    }
  }
  return null;
}
