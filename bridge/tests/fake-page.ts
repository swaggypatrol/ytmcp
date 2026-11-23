import { JSDOM } from "jsdom";
import { vi } from "vitest";
import { isKindAllowedForRole, validateEnvelopeObject, type Envelope } from "@ytm-glass/protocol";
import { installPageShim } from "../src/page-shim.js";
import type { BridgeTransport } from "../src/types.js";

/**
 * Synthetic model of the music.youtube.com structures the shim reads, shaped after what was
 * observed on the live page: `#movie_player` player API, the redux `queue` state behind
 * `ytmusic-player-bar.getState()`, `#contents` queue rows whose `data` is the renderer, the
 * menu-service-item queue-add behavior and `ytcfg`. All ids and titles are synthetic.
 */

export interface FakeSong {
  readonly videoId: string;
  readonly title?: string;
  readonly length?: string;
  readonly psv?: string;
  readonly playlistId?: string;
  readonly watchIndex?: number;
}

interface Renderer {
  videoId: string;
  title: { runs: { text: string }[] };
  longBylineText: { runs: unknown[] };
  lengthText: { runs: { text: string }[] };
  thumbnail: { thumbnails: { url: string; width: number }[] };
  playlistSetVideoId?: string;
  navigationEndpoint: { watchEndpoint: { videoId: string; playlistId?: string; index?: number; playlistSetVideoId?: string } };
}

export function createTransport() {
  const sent: Envelope[] = [];
  const listeners = new Set<(env: Envelope) => void>();
  const transport: BridgeTransport = {
    sendEnvelope(envelope) {
      const check = validateEnvelopeObject(envelope);
      if (!check.ok) throw new Error(`invalid envelope ${envelope.kind}: ${check.message}`);
      if (!isKindAllowedForRole(envelope.kind, "playerBridge")) throw new Error(`role forbids ${envelope.kind}`);
      sent.push(envelope);
      return { ok: true };
    },
    onEnvelope(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  let seq = 0;
  const command = (op: string, args: Record<string, unknown> | null = null, commandId = `cmd-${op}-${seq + 1}`) => {
    seq += 1;
    for (const l of [...listeners]) {
      l({ protocolVersion: 1, kind: "player.command", coreEpoch: null, accountEpoch: null, bridgeEpoch: null, requestId: `req-${seq}`, seq, payload: { commandId, op, args } } as Envelope);
    }
    return commandId;
  };
  const snapshotRequest = () => {
    seq += 1;
    for (const l of [...listeners]) {
      l({ protocolVersion: 1, kind: "player.snapshotRequest", coreEpoch: null, accountEpoch: null, bridgeEpoch: null, requestId: `req-${seq}`, seq, payload: {} } as Envelope);
    }
  };
  const of = <T>(kind: string) => sent.filter((e) => e.kind === kind).map((e) => e.payload as T);
  const last = <T>(kind: string) => of<T>(kind).at(-1);
  return { transport, sent, command, snapshotRequest, of, last, listenerCount: () => listeners.size };
}

export function createFakePage(options: {
  url?: string;
  songs?: FakeSong[];
  datasyncId?: string | null;
  deferQueueAddHandler?: boolean;
  queueEdits?: boolean;
  autoplay?: boolean;
  /** Place the autoplay switch inside the queue element, as older page layouts did. */
  legacyAutoplaySwitch?: boolean;
} = {}) {
  const dom = new JSDOM(
    `<!doctype html><html><body><ytmusic-app>
      <div id="movie_player"><video></video></div>
      <ytmusic-player-bar>
        <button class="previous-button"></button><button class="next-button"></button>
        <button class="shuffle"></button><button class="repeat"></button>
      </ytmusic-player-bar>
      <ytmusic-player-queue id="queue"><div id="contents"></div></ytmusic-player-queue>
      <ytmusic-player-page id="player-page"><div id="side-panel"><ytmusic-tab-renderer id="tab-renderer"><div><div id="tab-header"></div></div></ytmusic-tab-renderer></div></ytmusic-player-page>
    </ytmusic-app></body></html>`,
    { url: options.url ?? "https://music.youtube.com/watch?v=song00000a1" }
  );
  const win = dom.window;
  const doc = win.document;

  // Chromium same-window postMessage semantics (jsdom leaves source/origin empty).
  win.postMessage = ((data: unknown, targetOrigin?: string) => {
    if (targetOrigin && targetOrigin !== "*" && targetOrigin !== win.location.origin) return;
    queueMicrotask(() => win.dispatchEvent(new win.MessageEvent("message", { data, origin: win.location.origin, source: win })));
  }) as typeof win.postMessage;
  Object.defineProperty(win, "crypto", { configurable: true, value: globalThis.crypto });

  const datasyncId = options.datasyncId === undefined ? "synthDatasync0000000001||" : options.datasyncId;
  const cfg: Record<string, unknown> = { LOGGED_IN: datasyncId !== null, DATASYNC_ID: datasyncId ?? undefined };
  (win as unknown as { ytcfg: unknown }).ytcfg = { get: (k: string) => cfg[k] };

  let nextIndex = 0;
  const renderer = (s: FakeSong): Renderer => {
    const index = nextIndex++;
    return {
      videoId: s.videoId,
      title: { runs: [{ text: s.title ?? `Title ${s.videoId}` }] },
      longBylineText: {
        runs: [
          { text: "Synthetic Artist", navigationEndpoint: { browseEndpoint: { browseEndpointContextSupportedConfigs: { browseEndpointContextMusicConfig: { pageType: "MUSIC_PAGE_TYPE_ARTIST" } } } } },
          { text: " • " },
          { text: "Synthetic Album", navigationEndpoint: { browseEndpoint: { browseEndpointContextSupportedConfigs: { browseEndpointContextMusicConfig: { pageType: "MUSIC_PAGE_TYPE_ALBUM" } } } } },
        ],
      },
      lengthText: { runs: [{ text: s.length ?? "3:00" }] },
      thumbnail: { thumbnails: [{ url: `https://example.invalid/${s.videoId}.jpg`, width: 120 }] },
      playlistSetVideoId: s.psv ?? `psv-${index}`,
      navigationEndpoint: { watchEndpoint: { videoId: s.videoId, playlistId: s.playlistId, index: s.watchIndex ?? index } },
    };
  };

  const queue = {
    items: (options.songs ?? [{ videoId: "song00000a1" }, { videoId: "song00000b2" }]).map((s) => ({ playlistPanelVideoRenderer: renderer(s) })),
    selectedItemIndex: 0,
    isInfinite: false,
    automixItems: [] as unknown[],
    autoplay: options.autoplay,
    shuffleEnabled: false,
    repeatMode: "NONE",
  };
  const rendererAt = (i: number) => queue.items[i]!.playlistPanelVideoRenderer;

  const player = {
    state: 2,
    timeSec: 0,
    cpnCounter: 1,
    volume: 100,
    muted: false,
    lengthSec: 180 as number | null,
    videoId: rendererAt(0).videoId,
  };
  const loadIndex = (i: number) => {
    queue.selectedItemIndex = i;
    player.videoId = rendererAt(i).videoId;
    player.timeSec = 0;
    player.cpnCounter += 1;
  };

  const api = {
    getPlayerState: vi.fn(() => player.state),
    getCurrentTime: vi.fn(() => player.timeSec),
    getVideoData: vi.fn(() => ({ video_id: player.videoId })),
    getPlayerResponse: vi.fn(() => ({ videoDetails: { videoId: player.videoId, title: "From details", lengthSeconds: player.lengthSec === null ? undefined : String(player.lengthSec) } })),
    getCpn: vi.fn(() => `cpn-${player.cpnCounter}`),
    getPlaybackRate: vi.fn(() => 1),
    getStatsForNerds: vi.fn((): unknown => null),
    getPlaybackQuality: vi.fn((): unknown => null),
    getVolume: vi.fn(() => player.volume),
    isMuted: vi.fn(() => player.muted),
    playVideo: vi.fn(() => { player.state = 1; }),
    pauseVideo: vi.fn(() => { player.state = 2; }),
    seekTo: vi.fn((sec: number) => { player.timeSec = sec; }),
    setVolume: vi.fn((v: number) => { player.volume = v; }),
    mute: vi.fn(() => { player.muted = true; }),
    unMute: vi.fn(() => { player.muted = false; }),
    nextVideo: vi.fn(),
    previousVideo: vi.fn(),
  };
  Object.assign(doc.getElementById("movie_player")!, api);

  const playerPage = { playerPageWatchNextResponse: undefined as unknown };
  const bar = doc.querySelector("ytmusic-player-bar")!;
  (bar as unknown as { getState: () => unknown }).getState = () => ({ queue, playerPage });
  const click = (sel: string, fn: () => void) => bar.querySelector(sel)!.addEventListener("click", fn);
  click(".next-button", () => loadIndex(Math.min(queue.selectedItemIndex + 1, queue.items.length - 1)));
  click(".previous-button", () => {
    if (player.timeSec > 3) player.timeSec = 0; // YouTube Music restarts the track first.
    else loadIndex(Math.max(queue.selectedItemIndex - 1, 0));
  });
  click(".shuffle", () => { queue.shuffleEnabled = !queue.shuffleEnabled; });
  click(".repeat", () => { queue.repeatMode = queue.repeatMode === "NONE" ? "ALL" : queue.repeatMode === "ALL" ? "ONE" : "NONE"; });

  const contents = doc.querySelector("ytmusic-player-queue #contents")!;
  const renderQueue = () => {
    contents.replaceChildren(
      ...queue.items.map((item, i) => {
        const row = doc.createElement("ytmusic-player-queue-item");
        (row as unknown as { data: unknown }).data = item.playlistPanelVideoRenderer;
        const btn = doc.createElement("ytmusic-play-button-renderer");
        btn.addEventListener("click", () => loadIndex(i));
        row.append(btn);
        return row;
      })
    );
  };
  renderQueue();

  const queueDispatch = vi.fn((action: { type: "REMOVE_ITEM"; payload: number } | {
    type: "MOVE_ITEM"; payload: { fromIndex: number; toIndex: number };
  }) => {
    const selected = queue.items[queue.selectedItemIndex];
    if (action.type === "REMOVE_ITEM") {
      queue.items.splice(action.payload, 1);
    } else {
      const [item] = queue.items.splice(action.payload.fromIndex, 1);
      queue.items.splice(action.payload.toIndex, 0, item!);
    }
    queue.selectedItemIndex = queue.items.indexOf(selected!);
    renderQueue();
  });
  const queueStore = { getState: () => ({ queue }) };
  const queueNative = { store: { store: queueStore }, continuation: undefined as string | undefined };
  const queueElement = doc.getElementById("queue")!;
  if (options.queueEdits || options.autoplay !== undefined) {
    Object.assign(queueElement, { dispatch: queueDispatch, queue: queueNative });
  }
  const toggleAutoplay = vi.fn(() => { queue.autoplay = !queue.autoplay; });
  if (options.autoplay !== undefined) {
    const toggle = doc.createElement("button");
    toggle.id = "automix";
    toggle.addEventListener("click", toggleAutoplay);
    // The current official layout renders the switch in the player page's side-panel tab header.
    (options.legacyAutoplaySwitch ? queueElement : doc.getElementById("tab-header")!).append(toggle);
  }

  const navigate = vi.fn();
  (doc.querySelector("ytmusic-app") as unknown as { inst: unknown }).inst = { handleNavigationEndpoint: navigate };

  const queueAdd = vi.fn(async (endpoint: { queueAddEndpoint: { queueTarget: { videoId: string }; queueInsertPosition: string } }) => {
    const { queueTarget, queueInsertPosition } = endpoint.queueAddEndpoint;
    const item = { playlistPanelVideoRenderer: renderer({ videoId: queueTarget.videoId }) };
    const at = queueInsertPosition === "INSERT_AFTER_CURRENT_VIDEO" ? queue.selectedItemIndex + 1 : queue.items.length;
    queue.items.splice(at, 0, item);
    renderQueue();
  });
  const registerQueueAddHandler = () => win.customElements.define(
    "ytmusic-menu-service-item-renderer",
    class extends win.HTMLElement {
      inst = { handleQueueAddEndpoint: queueAdd };
    }
  );
  if (!options.deferQueueAddHandler) registerQueueAddHandler();

  const media = doc.querySelector("video")!;
  const mediaProps = { currentTime: 0, duration: Number.NaN, paused: true, volume: 1, muted: false, ended: false, readyState: 4 };
  for (const [k, v] of Object.entries(mediaProps)) {
    Object.defineProperty(media, k, { configurable: true, writable: true, value: v });
  }
  const fire = (type: string) => media.dispatchEvent(new win.Event(type));

  installPageShim(win as unknown as Window);
  return { dom, win: win as unknown as Window, doc, api, player, playerPage, queue, cfg, media, fire, navigate, queueAdd, registerQueueAddHandler, queueDispatch, queueStore, queueNative, toggleAutoplay, renderQueue, loadIndex };
}

export function flush(ms = 0): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Lets the bridge -> shim -> bridge round trips and coalesced refreshes settle. */
export async function settle(): Promise<void> {
  for (let i = 0; i < 6; i++) await flush(0);
}
