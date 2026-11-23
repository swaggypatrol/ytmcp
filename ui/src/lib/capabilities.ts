import type { AuthStatusInfo, CapabilitySupport, PlaybackStatus, PlayerConnectionState } from "@ytm-glass/protocol";
import { t } from "./i18n/index.svelte.js";

/** Whether a control may be used, and the reason shown when it is disabled or unverified. */
export interface Gate {
  readonly enabled: boolean;
  readonly reason: string | null;
}

/**
 * Player commands: `supported` works; `unverified` is allowed because the command lifecycle
 * reports the observed outcome; `unsupported` (or unknown) is disabled. When a player
 * connection state is provided and the bridge is disconnected or incompatible, commands are
 * gated off immediately rather than failing after dispatch.
 */
export function commandGate(
  support: CapabilitySupport | undefined,
  connection?: PlayerConnectionState | null
): Gate {
  if (connection === "disconnected" || connection === "incompatible") {
    return { enabled: false, reason: t.gates.bridgeDisconnected };
  }
  if (support === "supported") return { enabled: true, reason: null };
  if (support === "unverified") return { enabled: true, reason: t.gates.commandUnverified };
  return { enabled: false, reason: t.gates.unsupported };
}

/**
 * The play/pause toggle. Pausing follows `commandGate`; a toggle that would start playback
 * (core resolves it to play unless playing or buffering) also needs a loaded track, because
 * core rejects a play with nothing loaded.
 */
export function playPauseGate(
  support: CapabilitySupport | undefined,
  connection: PlayerConnectionState | null | undefined,
  playback: PlaybackStatus | null | undefined,
  hasTrack: boolean
): Gate {
  const gate = commandGate(support, connection);
  const wouldPlay = playback !== "playing" && playback !== "buffering";
  if (gate.enabled && wouldPlay && !hasTrack) return { enabled: false, reason: t.player.noTrackToPlay };
  return gate;
}

/**
 * Account writes need an authenticated account whose player page and catalog identity match,
 * and a capability that is proven `supported`; `unverified` stays disabled.
 */
export function writeGate(support: CapabilitySupport | undefined, auth: AuthStatusInfo | null | undefined): Gate {
  if (auth?.state !== "authenticated" || !auth.accountEpoch) {
    return { enabled: false, reason: t.gates.signInRequired };
  }
  if (!auth.playerCatalogMatched) {
    return { enabled: false, reason: t.gates.accountUnmatched };
  }
  if (support === "supported") return { enabled: true, reason: null };
  if (support === "unverified") return { enabled: false, reason: t.gates.writeUnverified };
  return { enabled: false, reason: t.gates.unsupported };
}
