import type { Envelope, PROTOCOL_VERSION } from "@ytm-glass/protocol";
import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import {
  IPC_CHANNELS,
  TRAY_PLAYER_OPS,
  type HostRuntimeInfo,
  type IpcAckResult,
} from "./channels.js";

export interface YtmGlassUiBridge {
  sendEnvelope(envelope: Envelope): Promise<IpcAckResult>;
  onEnvelope(listener: (envelope: Envelope) => void): () => void;
  getHostInfo(): Promise<HostRuntimeInfo>;
  getSandboxStatus(): { readonly sandboxed: boolean };
}

function isTopLevelUiFrame(): boolean {
  return typeof window !== "undefined" && window === window.top;
}

if (isTopLevelUiFrame()) {
  const api: YtmGlassUiBridge = {
    sendEnvelope(envelope: Envelope): Promise<IpcAckResult> {
      if (!isTopLevelUiFrame()) {
        return Promise.resolve({
          ok: false,
          code: "STALE_CONTEXT",
          message: "UI preload rejected non-top-level call",
        });
      }
      return ipcRenderer.invoke(IPC_CHANNELS.UI_SEND, envelope) as Promise<IpcAckResult>;
    },

    onEnvelope(listener: (envelope: Envelope) => void): () => void {
      if (typeof listener !== "function") {
        throw new TypeError("onEnvelope listener must be a function");
      }
      const wrapped = (_event: IpcRendererEvent, envelope: Envelope): void => {
        if (isTopLevelUiFrame()) {
          listener(envelope);
        }
      };
      ipcRenderer.on(IPC_CHANNELS.UI_EVENT, wrapped);
      return () => {
        ipcRenderer.removeListener(IPC_CHANNELS.UI_EVENT, wrapped);
      };
    },

    getHostInfo(): Promise<HostRuntimeInfo> {
      return ipcRenderer.invoke(IPC_CHANNELS.UI_HOST_INFO) as Promise<HostRuntimeInfo>;
    },

    getSandboxStatus(): { readonly sandboxed: boolean } {
      return {
        sandboxed: Boolean((process as NodeJS.Process & { sandboxed?: boolean }).sandboxed),
      };
    },
  };

  contextBridge.exposeInMainWorld("ytmGlassUi", Object.freeze(api));

  // Tray playback actions enter the core through the same ui.playerCommand path (and the
  // same host IPC guard) as the UI's own buttons; the host never issues UI commands itself.
  // Literal kept out of the runtime import (the protocol package pulls in the schema validator);
  // the type annotation makes tsc fail if the protocol version changes.
  const protocolVersion: typeof PROTOCOL_VERSION = 1;
  let trayCommandCounter = 0;
  ipcRenderer.on(IPC_CHANNELS.UI_TRAY_COMMAND, (_event: IpcRendererEvent, op: unknown) => {
    if (!isTopLevelUiFrame() || !(TRAY_PLAYER_OPS as readonly unknown[]).includes(op)) {
      return;
    }
    trayCommandCounter += 1;
    const id = `tray-${Date.now()}-${trayCommandCounter}`;
    const envelope: Envelope = {
      protocolVersion,
      kind: "ui.playerCommand",
      coreEpoch: null,
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: id,
      seq: trayCommandCounter,
      payload: { commandId: id, op: op as (typeof TRAY_PLAYER_OPS)[number] },
    };
    void ipcRenderer.invoke(IPC_CHANNELS.UI_SEND, envelope);
  });
}
