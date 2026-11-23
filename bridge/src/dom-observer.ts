export const MEDIA_EVENTS: readonly string[] = [
  "play",
  "pause",
  "playing",
  "waiting",
  "stalled",
  "timeupdate",
  "seeked",
  "ended",
  "durationchange",
  "volumechange",
  "ratechange",
  "error",
];

export function findPrimaryMediaElement(doc: Document): HTMLMediaElement | null {
  return doc.querySelector<HTMLMediaElement>("#movie_player video, video.html5-main-video, ytmusic-player video");
}

export interface DomObserverCallbacks {
  readonly onMediaEvent: (eventType: string) => void;
  readonly onDomChanged: () => void;
  readonly isActive: () => boolean;
}

/**
 * Change triggers from the shared DOM, usable from any JS world: HTML5 media events and
 * debounced, bounded MutationObservers on the player bar and queue. It reads no page
 * data itself; the bridge fetches facts from the page shim when a trigger fires.
 */
export class DomObserver {
  private readonly win: Window;
  private readonly doc: Document;
  private readonly callbacks: DomObserverCallbacks;
  private readonly debounceMs: number;
  private boundMedia: HTMLMediaElement | null = null;
  private mediaTeardown: (() => void) | null = null;
  private observers: MutationObserver[] = [];
  private debounceTimer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;

  public constructor(win: Window, doc: Document, callbacks: DomObserverCallbacks, debounceMs: number) {
    this.win = win;
    this.doc = doc;
    this.callbacks = callbacks;
    this.debounceMs = debounceMs;
  }

  public getMediaElement(): HTMLMediaElement | null {
    if (this.boundMedia && !this.boundMedia.isConnected) this.unbindMedia();
    if (!this.boundMedia) {
      const candidate = findPrimaryMediaElement(this.doc);
      if (candidate) this.bindMedia(candidate);
    }
    return this.boundMedia;
  }

  /** (Re)binds the media element and observers; called at start and after SPA navigation. */
  public rebind(): void {
    if (this.disposed || !this.callbacks.isActive()) return;
    const candidate = findPrimaryMediaElement(this.doc);
    if (candidate !== this.boundMedia) {
      this.unbindMedia();
      if (candidate) this.bindMedia(candidate);
    }
    this.disconnectObservers();
    const Ctor = (this.win as unknown as { MutationObserver?: typeof MutationObserver }).MutationObserver;
    if (typeof Ctor !== "function") return;

    const schedule = (): void => {
      if (this.disposed || this.debounceTimer !== null) return;
      this.debounceTimer = setTimeout(() => {
        this.debounceTimer = null;
        if (this.disposed || !this.callbacks.isActive()) return;
        if (findPrimaryMediaElement(this.doc) !== this.boundMedia) this.rebind();
        this.callbacks.onDomChanged();
      }, this.debounceMs);
    };
    const observe = (target: Node, options: MutationObserverInit): void => {
      const obs = new Ctor(schedule);
      obs.observe(target, options);
      this.observers.push(obs);
    };

    const playerBar = this.doc.querySelector("ytmusic-player-bar");
    const queue = this.doc.querySelector("ytmusic-player-queue");
    // No characterData: the progress text changes every second and must not drive sampling.
    if (playerBar) {
      observe(playerBar, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ["title", "aria-label", "aria-pressed", "disabled", "aria-disabled", "like-status"],
      });
    }
    if (queue) {
      observe(queue, { childList: true, subtree: true, attributes: true, attributeFilter: ["selected", "play-button-state"] });
    }
    // Lazy mount of the player bar, queue or media element.
    const mountRoot = this.doc.querySelector("ytmusic-app") ?? this.doc.body;
    if (mountRoot && (!playerBar || !queue || !candidate)) {
      observe(mountRoot, { childList: true, subtree: true });
    }
  }

  public dispose(): void {
    this.disposed = true;
    if (this.debounceTimer !== null) clearTimeout(this.debounceTimer);
    this.debounceTimer = null;
    this.unbindMedia();
    this.disconnectObservers();
  }

  private bindMedia(media: HTMLMediaElement): void {
    this.boundMedia = media;
    const handlers = MEDIA_EVENTS.map((name) => {
      const fn = (): void => {
        if (!this.disposed && this.callbacks.isActive()) this.callbacks.onMediaEvent(name);
      };
      media.addEventListener(name, fn);
      return [name, fn] as const;
    });
    this.mediaTeardown = () => {
      for (const [name, fn] of handlers) media.removeEventListener(name, fn);
    };
  }

  private unbindMedia(): void {
    this.mediaTeardown?.();
    this.mediaTeardown = null;
    this.boundMedia = null;
  }

  private disconnectObservers(): void {
    for (const obs of this.observers) obs.disconnect();
    this.observers = [];
  }
}
