import { vi } from "vitest";

type Listener = (...args: unknown[]) => unknown;

/** Minimal event emitter matching the subset of Electron's emitter API the host uses. */
export class FakeEmitter {
  private readonly listeners = new Map<string, Listener[]>();

  public on(event: string, fn: Listener): this {
    this.listeners.set(event, [...(this.listeners.get(event) ?? []), fn]);
    return this;
  }

  public once(event: string, fn: Listener): this {
    const wrapped: Listener = (...args) => {
      this.removeListener(event, wrapped);
      return fn(...args);
    };
    return this.on(event, wrapped);
  }

  public removeListener(event: string, fn: Listener): this {
    this.listeners.set(
      event,
      (this.listeners.get(event) ?? []).filter((l) => l !== fn)
    );
    return this;
  }

  public emit(event: string, ...args: unknown[]): void {
    for (const fn of [...(this.listeners.get(event) ?? [])]) {
      fn(...args);
    }
  }

  public removeAllListeners(): void {
    this.listeners.clear();
  }
}

export interface FakeCloseEvent {
  defaultPrevented: boolean;
  preventDefault(): void;
}

export function makeEvent(): FakeCloseEvent {
  return {
    defaultPrevented: false,
    preventDefault() {
      this.defaultPrevented = true;
    },
  };
}

export class FakeWebContents extends FakeEmitter {
  public url = "";
  public destroyed = false;
  public readonly mainFrame = { origin: "app://ui", url: "app://ui/", parent: null };
  public readonly send = vi.fn();
  public readonly setWindowOpenHandler = vi.fn<(handler: (details: { url: string }) => unknown) => void>();
  public readonly setAudioMuted = vi.fn();
  public readonly setZoomFactor = vi.fn();
  public isDestroyed(): boolean {
    return this.destroyed;
  }
  public getURL(): string {
    return this.url;
  }
  public isLoadingMainFrame(): boolean {
    return false;
  }
  public crashed = false;
  public isCrashed(): boolean {
    return this.crashed;
  }
  public readonly reload = vi.fn();
}

export class FakeBrowserWindow extends FakeEmitter {
  public static instances: FakeBrowserWindow[] = [];
  public readonly webContents = new FakeWebContents();
  public visible: boolean;
  public focused = false;
  public minimized = false;
  public focusAllowed = true;
  public destroyed = false;
  public readonly flashFrame = vi.fn();

  public constructor(public readonly options: { show?: boolean; webPreferences?: { preload?: string } }) {
    super();
    this.visible = options.show ?? true;
    FakeBrowserWindow.instances.push(this);
  }

  public readonly loadURL = vi.fn(async (url: string) => {
    this.webContents.url = url;
  });
  public isDestroyed(): boolean {
    return this.destroyed;
  }
  public isVisible(): boolean {
    return this.visible;
  }
  public isMinimized(): boolean {
    return this.minimized;
  }
  public isFocused(): boolean {
    return this.focused;
  }
  public restore(): void {}
  public show(): void {
    this.visible = true;
    this.emit("show");
  }
  public hide(): void {
    this.visible = false;
    this.emit("hide");
  }
  public focus(): void {
    this.focused = this.focusAllowed;
  }
  /** Shows without activating: focus is unchanged. */
  public readonly showInactive = vi.fn(() => {
    this.visible = true;
    this.emit("show");
  });
  /** Emulates a user/WM close request: `close` listeners may cancel it. */
  public close(): FakeCloseEvent {
    const event = makeEvent();
    this.emit("close", event);
    if (!event.defaultPrevented) {
      this.destroy();
    }
    return event;
  }
  public destroy(): void {
    if (this.destroyed) {
      return;
    }
    this.destroyed = true;
    this.webContents.destroyed = true;
    this.visible = false;
    this.emit("closed");
  }
}

export interface FakeSession {
  readonly partition: string;
  readonly protocolHandlers: Map<string, (request: Request) => Promise<Response>>;
  readonly protocol: { handle: (scheme: string, handler: (request: Request) => Promise<Response>) => void };
  readonly cookies: {
    get: ReturnType<typeof vi.fn>;
    on: ReturnType<typeof vi.fn>;
    removeListener: ReturnType<typeof vi.fn>;
  };
  readonly setPermissionRequestHandler: ReturnType<typeof vi.fn>;
  readonly setPermissionCheckHandler: ReturnType<typeof vi.fn>;
  readonly clearStorageData: ReturnType<typeof vi.fn>;
  readonly clearCache: ReturnType<typeof vi.fn>;
  readonly clearAuthCache: ReturnType<typeof vi.fn>;
}

function makeSession(partition: string): FakeSession {
  const protocolHandlers = new Map<string, (request: Request) => Promise<Response>>();
  return {
    partition,
    protocolHandlers,
    protocol: {
      handle: (scheme, handler) => {
        protocolHandlers.set(scheme, handler);
      },
    },
    cookies: {
      get: vi.fn(async () => []),
      on: vi.fn(),
      removeListener: vi.fn(),
    },
    setPermissionRequestHandler: vi.fn(),
    setPermissionCheckHandler: vi.fn(),
    clearStorageData: vi.fn(async () => {}),
    clearCache: vi.fn(async () => {}),
    clearAuthCache: vi.fn(async () => {}),
  };
}

export class FakeTray {
  public static instances: FakeTray[] = [];
  public menuTemplate: Array<{ label?: string; type?: string; click?: () => void }> = [];
  public readonly destroy = vi.fn();
  public constructor() {
    FakeTray.instances.push(this);
  }
  public setToolTip(): void {}
  public setContextMenu(menu: { template: FakeTray["menuTemplate"] }): void {
    this.menuTemplate = menu.template;
  }
  public on(): void {}
  public click(label: string): void {
    const item = this.menuTemplate.find((entry) => entry.label === label);
    if (!item?.click) {
      throw new Error(`No tray menu item labelled ${label}`);
    }
    item.click();
  }
}

export class FakeNotification extends FakeEmitter {
  public static instances: FakeNotification[] = [];
  public static supported = true;
  public static isSupported(): boolean {
    return FakeNotification.supported;
  }
  public shown = false;
  public closed = false;
  public constructor(public readonly options: Record<string, unknown>) {
    super();
    FakeNotification.instances.push(this);
  }
  public show(): void {
    this.shown = true;
  }
  public close(): void {
    this.closed = true;
    this.emit("close");
  }
  /** Names of events the host subscribed to (a click handler could raise a window). */
  public listenedEvents(): string[] {
    return [...(this as unknown as { listeners: Map<string, unknown[]> }).listeners.entries()]
      .filter(([, fns]) => fns.length > 0)
      .map(([event]) => event);
  }
}

/** Shared mutable state behind the `electron` mock; call `reset()` before each test. */
function createFakeElectron() {
  const appEvents = new FakeEmitter();
  const sessions = new Map<string, FakeSession>();
  const ipcHandlers = new Map<string, (...args: unknown[]) => unknown>();
  const app = {
    isPackaged: false,
    isReady: () => true,
    requestSingleInstanceLock: vi.fn(() => true),
    setName: vi.fn(),
    setDesktopName: vi.fn(),
    setPath: vi.fn(),
    whenReady: async () => undefined,
    getGPUFeatureStatus: () => ({ gpu_compositing: "enabled" }),
    getGPUInfo: async () => ({}),
    on: (event: string, fn: Listener) => appEvents.on(event, fn),
    removeListener: (event: string, fn: Listener) => appEvents.removeListener(event, fn),
    quit: vi.fn(),
    exit: vi.fn(),
  };
  const electron = {
    app,
    BrowserWindow: FakeBrowserWindow,
    Tray: FakeTray,
    Notification: FakeNotification,
    Menu: { buildFromTemplate: (template: FakeTray["menuTemplate"]) => ({ template }) },
    nativeImage: { createFromDataURL: () => ({}) },
    shell: { openExternal: vi.fn(async () => {}) },
    dialog: { showErrorBox: vi.fn() },
    powerMonitor: new FakeEmitter(),
    protocol: { registerSchemesAsPrivileged: vi.fn() },
    ipcMain: {
      handle: (channel: string, fn: (...args: unknown[]) => unknown) => {
        ipcHandlers.set(channel, fn);
      },
    },
    net: { isOnline: () => true, fetch: vi.fn(async () => new Response("cover-bytes", { status: 200 })) },
    session: {
      fromPartition: (partition: string) => {
        let s = sessions.get(partition);
        if (!s) {
          s = makeSession(partition);
          sessions.set(partition, s);
        }
        return s;
      },
    },
    safeStorage: {
      isEncryptionAvailable: () => true,
      getSelectedStorageBackend: () => "kwallet6",
    },
  };
  const reset = (): void => {
    appEvents.removeAllListeners();
    sessions.clear();
    ipcHandlers.clear();
    FakeBrowserWindow.instances = [];
    FakeTray.instances = [];
    FakeNotification.instances = [];
    FakeNotification.supported = true;
    vi.clearAllMocks();
  };
  return { electron, appEvents, sessions, ipcHandlers, reset };
}

export const fake = createFakeElectron();
