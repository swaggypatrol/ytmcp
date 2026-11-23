import type { Envelope } from "@ytm-glass/protocol";
import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import { BRIDGE_WORLD_ID, IPC_CHANNELS, MUSIC_ORIGIN, type IpcAckResult } from "./channels.js";

export interface YtmGlassPlayerBridge {
  sendEnvelope(envelope: Envelope): Promise<IpcAckResult>;
  onEnvelope(listener: (envelope: Envelope) => void): () => void;
}

function isTrustedMusicTopFrame(): boolean {
  return (
    typeof window !== "undefined" &&
    window === window.top &&
    window.location.origin === MUSIC_ORIGIN
  );
}

// NEVER expose or register bridge IPC outside the exact top-level music.youtube.com origin,
// and never in the page main world: page scripts could otherwise forge any bridge.* frame.
if (isTrustedMusicTopFrame()) {
  const api: YtmGlassPlayerBridge = {
    sendEnvelope(envelope: Envelope): Promise<IpcAckResult> {
      if (!isTrustedMusicTopFrame()) {
        return Promise.resolve({
          ok: false,
          code: "STALE_CONTEXT",
          message: "Player bridge rejected outside music.youtube.com top frame",
        });
      }
      return ipcRenderer.invoke(IPC_CHANNELS.BRIDGE_SEND, envelope) as Promise<IpcAckResult>;
    },

    onEnvelope(listener: (envelope: Envelope) => void): () => void {
      if (typeof listener !== "function") {
        throw new TypeError("onEnvelope listener must be a function");
      }
      const wrapped = (_event: IpcRendererEvent, envelope: Envelope): void => {
        if (isTrustedMusicTopFrame()) {
          listener(envelope);
        }
      };
      ipcRenderer.on(IPC_CHANNELS.BRIDGE_EVENT, wrapped);
      return () => {
        ipcRenderer.removeListener(IPC_CHANNELS.BRIDGE_EVENT, wrapped);
      };
    },
  };

  contextBridge.exposeInIsolatedWorld(BRIDGE_WORLD_ID, "ytmGlassBridge", Object.freeze(api));
}
