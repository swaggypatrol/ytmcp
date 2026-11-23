import { execFile } from "node:child_process";
import { Menu, Tray, nativeImage } from "electron";
import type { TrayPlayerOp } from "../../preload/channels.js";
import { TRAY_ICON_PNG_BASE64 } from "./tray-icon.js";

export interface TrayActions {
  readonly toggleMainWindow: () => void;
  readonly playerCommand: (op: TrayPlayerOp) => void;
  readonly openPlayerWindow: () => void;
  readonly quit: () => void;
}

export interface AppTray {
  /** Updates the play/pause entry from the core's observed playback state. */
  setPlaying(playing: boolean): void;
  destroy(): void;
}

const STATUS_NOTIFIER_WATCHER = "org.kde.StatusNotifierWatcher";
/** No change signal is consumed (that would need a resident D-Bus client), so the watcher is polled. */
export const STATUS_NOTIFIER_POLL_MS = 10_000;

/**
 * Outcome of one probe. `absent` is a definitive bus answer (no watcher owner, or no host
 * registered); `error` is a failed probe (spawn failure, timeout under load, transient bus
 * error) that says nothing about the tray host.
 */
export type StatusNotifierProbe =
  | { readonly kind: "available"; readonly owner: string }
  | { readonly kind: "absent" }
  | { readonly kind: "error" };

/** D-Bus errors meaning the name has no owner: a real answer, not a probe failure. */
const NO_OWNER_ERROR_RE = /\b(NameHasNoOwner|ServiceUnknown)\b/;

type GdbusResult = { readonly ok: true; readonly out: string } | { readonly ok: false; readonly noOwner: boolean };

function gdbusCall(args: readonly string[], timeoutMs: number): Promise<GdbusResult> {
  return new Promise((resolveOutput) => {
    execFile("gdbus", ["call", "--session", ...args], { timeout: timeoutMs }, (error, stdout, stderr) => {
      if (!error) {
        resolveOutput({ ok: true, out: String(stdout) });
        return;
      }
      const timedOut = (error as { killed?: boolean }).killed === true;
      const text = `${error.message}\n${stderr === undefined ? "" : String(stderr)}`;
      resolveOutput({ ok: false, noOwner: !timedOut && NO_OWNER_ERROR_RE.test(text) });
    });
  });
}

/**
 * Probes for a StatusNotifierItem host (e.g. KDE Plasma's system tray): `available` carries
 * the unique bus name owning `org.kde.StatusNotifierWatcher` while a host is registered with
 * it, i.e. a tray icon would actually be visible. A new owner means plasmashell restarted.
 */
export async function probeStatusNotifierHost(timeoutMs = 2_000): Promise<StatusNotifierProbe> {
  const ownerRes = await gdbusCall(
    [
      "--dest", "org.freedesktop.DBus",
      "--object-path", "/org/freedesktop/DBus",
      "--method", "org.freedesktop.DBus.GetNameOwner",
      STATUS_NOTIFIER_WATCHER,
    ],
    timeoutMs
  );
  if (!ownerRes.ok) {
    return ownerRes.noOwner ? { kind: "absent" } : { kind: "error" };
  }
  const owner = ownerRes.out.match(/'(:[0-9.]+)'/)?.[1];
  if (!owner) {
    return { kind: "error" };
  }
  const registeredRes = await gdbusCall(
    [
      "--dest", STATUS_NOTIFIER_WATCHER,
      "--object-path", "/StatusNotifierWatcher",
      "--method", "org.freedesktop.DBus.Properties.Get",
      STATUS_NOTIFIER_WATCHER,
      "IsStatusNotifierHostRegistered",
    ],
    timeoutMs
  );
  if (!registeredRes.ok) {
    return registeredRes.noOwner ? { kind: "absent" } : { kind: "error" };
  }
  if (/\btrue\b/.test(registeredRes.out)) {
    return { kind: "available", owner };
  }
  return /\bfalse\b/.test(registeredRes.out) ? { kind: "absent" } : { kind: "error" };
}

/** Resolves true only when a StatusNotifierItem host is registered on the session bus. */
export async function isStatusNotifierHostAvailable(timeoutMs = 2_000): Promise<boolean> {
  return (await probeStatusNotifierHost(timeoutMs)).kind === "available";
}

/** Definitive "no host" answers in a row before a shown tray is given up. */
export const STATUS_NOTIFIER_ABSENT_CONFIRMATIONS = 2;

/**
 * Re-probes the tray host every `intervalMs` and reports each owner change starting from
 * `initialOwner`: a new owner at once (appeared or plasmashell restarted), its loss only after
 * consecutive definitive negatives. Failed probes keep the current owner. Probes never
 * overlap. Returns stop.
 */
export function watchStatusNotifierHost(
  initialOwner: string | null,
  onChange: (owner: string | null) => void,
  intervalMs = STATUS_NOTIFIER_POLL_MS,
  probe: () => Promise<StatusNotifierProbe> = () => probeStatusNotifierHost()
): () => void {
  let owner = initialOwner;
  let absentInARow = 0;
  let probing = false;
  let stopped = false;
  const timer = setInterval(() => {
    if (probing) {
      return;
    }
    probing = true;
    void probe()
      .catch((): StatusNotifierProbe => ({ kind: "error" }))
      .then((result) => {
        probing = false;
        if (stopped || result.kind === "error") {
          return;
        }
        if (result.kind === "available") {
          absentInARow = 0;
          if (result.owner !== owner) {
            owner = result.owner;
            onChange(owner);
          }
          return;
        }
        absentInARow += 1;
        if (owner !== null && absentInARow >= STATUS_NOTIFIER_ABSENT_CONFIRMATIONS) {
          owner = null;
          onChange(null);
        }
      });
  }, intervalMs);
  timer.unref?.();
  return () => {
    stopped = true;
    clearInterval(timer);
  };
}

/**
 * Creates the KDE StatusNotifierItem tray. Playback entries go through `playerCommand`,
 * which the host routes via the main UI's normal command path.
 */
export function createAppTray(tooltip: string, actions: TrayActions): AppTray {
  const tray = new Tray(nativeImage.createFromDataURL(`data:image/png;base64,${TRAY_ICON_PNG_BASE64}`));
  tray.setToolTip(tooltip);

  let playing = false;
  const rebuildMenu = (): void => {
    tray.setContextMenu(
      Menu.buildFromTemplate([
        { label: "Show / Hide Main Window", click: () => actions.toggleMainWindow() },
        { type: "separator" },
        { label: playing ? "Pause" : "Play", click: () => actions.playerCommand("toggle") },
        { label: "Previous", click: () => actions.playerCommand("previous") },
        { label: "Next", click: () => actions.playerCommand("next") },
        { type: "separator" },
        { label: "Open Official Player", click: () => actions.openPlayerWindow() },
        { label: "Quit", click: () => actions.quit() },
      ])
    );
  };
  rebuildMenu();
  tray.on("click", () => actions.toggleMainWindow());

  return {
    setPlaying(next: boolean): void {
      if (next !== playing) {
        playing = next;
        rebuildMenu();
      }
    },
    destroy(): void {
      tray.destroy();
    },
  };
}
