export const IPC_CHANNELS = {
  UI_SEND: "ytmcp:ui:send",
  UI_EVENT: "ytmcp:ui:event",
  UI_HOST_INFO: "ytmcp:ui:host-info",
  BRIDGE_SEND: "ytmcp:bridge:send",
  BRIDGE_EVENT: "ytmcp:bridge:event",
  /** host -> main UI preload: a tray playback action to issue as a ui.playerCommand. */
  UI_TRAY_COMMAND: "ytmcp:ui:tray-command",
} as const;

/** Playback actions the tray may ask the main UI to issue. */
export const TRAY_PLAYER_OPS = ["toggle", "next", "previous"] as const;
export type TrayPlayerOp = (typeof TRAY_PLAYER_OPS)[number];

export const MUSIC_ORIGIN = "https://music.youtube.com";

/**
 * Isolated world that holds the player bridge and its IPC API. The page main world only
 * gets the page shim (fixed music operations over postMessage), never `ytmcpBridge`.
 */
export const BRIDGE_WORLD_ID = 1001;

export interface IpcAckResult {
  ok: boolean;
  code?: string;
  message?: string;
}

export interface HostRuntimeInfo {
  appId: string;
  appScheme: string;
  displayName: string;
  mockMode: boolean;
  protocolVersion: number;
  version: string;
}
