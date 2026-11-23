import type { RepeatMode } from "@ytmcp/protocol";
import {
  isPlaylistEntryId,
  MUSIC_ORIGIN,
  PAGE_SHIM_CHANNEL,
  type PageControls,
  type PageQueue,
  type PageQueueItem,
  type PageShimErrorCode,
  type PageShimOp,
  type PageShimResponse,
  type PageSnapshot,
  type PageTrack,
} from "./types.js";

/**
 * Minimal fixed adapter that runs in the page main world, where YouTube Music's private
 * objects (player API, redux store, Polymer behaviors) live. It answers only the fixed
 * operations in `PageShimOp` over `window.postMessage`; it exposes no eval, no generic
 * property access and no network or credential access. The bridge itself may run in an
 * isolated world and reaches the page only through this shim and the shared DOM.
 */

const SHIM_CLEANUP_SYMBOL = Symbol.for("ytmcpPageShimCleanup");
const ACCOUNT_HINT_DOMAIN = "ytm-glass-account-v1\n";
const MAX_QUEUE_WINDOW = 200;
const MAX_CLEAR_UPCOMING = 50;
const QUEUE_WINDOW_BEFORE_CURRENT = 20;
const ENQUEUE_TIMEOUT_MS = 6_000;
const QUEUE_HANDLER_RETRY_MS = 500;
const NAVIGATE_SETTLE_MS = 5_000;
/** A load that sits unstarted, cued or paused this long did not autoplay (states -1, 5, 2). */
const STUCK_UNSTARTED_MS = 800;
/** How long after a skip the shim keeps checking that the new track actually started. */
const SKIP_START_WATCH_MS = 5_000;
const SAFE_ID_RE = /^[A-Za-z0-9_-]{1,128}$/;
const AUDIO_CODEC_STATS_RE = /^[a-z0-9.-]{1,40}(?: \(\d{1,4}\))? \/ (mp4a(?:\.[0-9a-f]{1,4}){1,3}|opus)(?: \(\d{1,4}\))?$/i;
const QUALITY_LABEL_RE = /^(?:auto|tiny|small|medium|large|hd[1-9]\d{2,3}|highres)$/;
const PLAYBACK_INPUT_SELECTOR = [
  "ytmusic-player-bar :is(#play-pause-button, .play-pause-button, .next-button, .previous-button)",
  "#movie_player .ytp-play-button",
  "ytmusic-player-queue ytmusic-play-button-renderer",
].join(", ");
const PLAYBACK_MEDIA_KEYS = new Set(["MediaPlayPause", "MediaPlay", "MediaPause", "MediaStop", "MediaTrackNext", "MediaTrackPrevious"]);

const ALLOWED_OPS: ReadonlySet<string> = new Set<PageShimOp>([
  "probe",
  "play",
  "pause",
  "seek",
  "next",
  "previous",
  "setVolume",
  "setMuted",
  "setShuffle",
  "setRepeat",
  "setAutoplay",
  "navigate",
  "selectQueueItem",
  "removeQueueItem",
  "moveQueueItem",
  "clearUpcoming",
  "enqueue",
]);

interface PlayerApi {
  playVideo?: () => void;
  pauseVideo?: () => void;
  seekTo?: (seconds: number, allowSeekAhead?: boolean) => void;
  nextVideo?: () => void;
  previousVideo?: () => void;
  setVolume?: (volume0To100: number) => void;
  getVolume?: () => number;
  mute?: () => void;
  unMute?: () => void;
  isMuted?: () => boolean;
  getPlayerState?: () => number;
  getCurrentTime?: () => number;
  getVideoLoadedFraction?: () => number;
  getPlaybackRate?: () => number;
  getCpn?: () => string;
  getVideoData?: () => { video_id?: string; title?: string; author?: string };
  getPlayerResponse?: () => { videoDetails?: VideoDetails } | null;
  getStatsForNerds?: () => unknown;
  getPlaybackQuality?: () => unknown;
}

interface VideoDetails {
  videoId?: string;
  title?: string;
  author?: string;
  lengthSeconds?: string;
  thumbnail?: { thumbnails?: Thumbnail[] };
}

interface Thumbnail {
  url?: string;
  width?: number;
}

interface TextRun {
  text?: string;
  navigationEndpoint?: {
    browseEndpoint?: {
      browseEndpointContextSupportedConfigs?: {
        browseEndpointContextMusicConfig?: { pageType?: string };
      };
    };
  };
}

interface VideoRenderer {
  videoId?: string;
  title?: { runs?: TextRun[] };
  longBylineText?: { runs?: TextRun[] };
  shortBylineText?: { runs?: TextRun[] };
  lengthText?: { runs?: TextRun[] };
  thumbnail?: { thumbnails?: Thumbnail[] };
  playlistSetVideoId?: string;
  unplayableText?: unknown;
  navigationEndpoint?: { watchEndpoint?: WatchEndpoint };
}

interface QueueState {
  items?: unknown[];
  automixItems?: unknown[];
  selectedItemIndex?: number;
  isInfinite?: boolean;
  shuffleEnabled?: boolean;
  repeatMode?: string;
  autoplay?: boolean;
}

interface WatchNextPanel {
  playlistId?: string;
  contents?: unknown[];
}

interface WatchEndpoint { videoId?: string; playlistId?: string; index?: number; playlistSetVideoId?: string }

interface WatchNextResponse {
  currentVideoEndpoint?: { watchEndpoint?: WatchEndpoint };
  contents?: {
    singleColumnMusicWatchNextResultsRenderer?: {
      tabbedRenderer?: {
        watchNextTabbedResultsRenderer?: {
          tabs?: { tabRenderer?: { content?: { musicQueueRenderer?: { content?: { playlistPanelRenderer?: WatchNextPanel } } } } }[];
        };
      };
    };
  };
}

interface PlayerStoreState {
  queue?: QueueState;
  playerPage?: { playerPageWatchNextResponse?: WatchNextResponse };
}

interface ReduxElement extends HTMLElement {
  getState?: () => PlayerStoreState;
}

type QueueEditAction =
  | { type: "REMOVE_ITEM"; payload: number }
  | { type: "MOVE_ITEM"; payload: { fromIndex: number; toIndex: number } };

interface QueueElement extends HTMLElement {
  dispatch?: (action: QueueEditAction) => unknown;
  queue?: {
    store?: { store?: { getState?: () => { queue?: QueueState } } };
    continuation?: string;
    autoPlaying?: boolean;
  };
}

interface QueueAddHandler {
  inst?: { handleQueueAddEndpoint?: (endpoint: unknown) => unknown };
}

type ShimResult =
  | { readonly ok: true; readonly data?: PageSnapshot }
  | { readonly ok: false; readonly error: PageShimErrorCode; readonly message: string };

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function isTrustedMusicTopFrame(win: Window): boolean {
  try {
    return win === win.top && win.location.origin === MUSIC_ORIGIN;
  } catch {
    return false;
  }
}

function finiteOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function call<T>(fn: (() => T) | undefined, thisArg: unknown): T | null {
  if (typeof fn !== "function") return null;
  try {
    return fn.call(thisArg);
  } catch {
    return null;
  }
}

function runsText(runs: TextRun[] | undefined): string {
  return Array.isArray(runs) ? runs.map((r) => (typeof r?.text === "string" ? r.text : "")).join("") : "";
}

function parseClockMs(text: string): number | null {
  const m = /^(?:(\d{1,2}):)?(\d{1,2}):(\d{2})$/.exec(text.trim());
  if (!m) return null;
  return ((m[1] ? Number(m[1]) : 0) * 3600 + Number(m[2]) * 60 + Number(m[3])) * 1000;
}

function pickCover(thumbnails: Thumbnail[] | undefined): string | null {
  if (!Array.isArray(thumbnails) || thumbnails.length === 0) return null;
  const usable = thumbnails.filter((t) => typeof t?.url === "string" && t.url.startsWith("https://"));
  const sized = usable.find((t) => (t.width ?? 0) >= 120) ?? usable[usable.length - 1];
  return sized?.url ?? null;
}

function pageType(run: TextRun): string | null {
  return (
    run.navigationEndpoint?.browseEndpoint?.browseEndpointContextSupportedConfigs
      ?.browseEndpointContextMusicConfig?.pageType ?? null
  );
}

/** Artists and album from a byline, using the typed browse links when the page provides them. */
function parseByline(runs: TextRun[] | undefined): { artists: string[]; albumTitle: string | null } {
  const list = Array.isArray(runs) ? runs : [];
  const artists = list
    .filter((r) => {
      const t = pageType(r);
      return t === "MUSIC_PAGE_TYPE_ARTIST" || t === "MUSIC_PAGE_TYPE_USER_CHANNEL";
    })
    .map((r) => (r.text ?? "").trim())
    .filter(Boolean);
  const album = list.find((r) => pageType(r) === "MUSIC_PAGE_TYPE_ALBUM")?.text?.trim() ?? null;
  if (artists.length > 0) return { artists, albumTitle: album || null };
  const first = runsText(list).split("•")[0] ?? "";
  return {
    artists: first.split(/,|&/).map((s) => s.trim()).filter(Boolean),
    albumTitle: album || null,
  };
}

function rendererOf(item: unknown): VideoRenderer | null {
  if (!isPlainObject(item)) return null;
  const direct = item.playlistPanelVideoRenderer;
  if (isPlainObject(direct)) return direct as VideoRenderer;
  const wrapper = item.playlistPanelVideoWrapperRenderer;
  if (isPlainObject(wrapper) && isPlainObject(wrapper.primaryRenderer)) {
    const inner = wrapper.primaryRenderer.playlistPanelVideoRenderer;
    if (isPlainObject(inner)) return inner as VideoRenderer;
  }
  return null;
}

function selectedRendererOf(state: QueueState | null): VideoRenderer | null {
  const index = state?.selectedItemIndex;
  return Array.isArray(state?.items) && typeof index === "number" && Number.isSafeInteger(index) && index >= 0
    ? rendererOf(state.items[index]) : null;
}

function isWatchIndex(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 0xffff_ffff;
}

function rendererPlaybackContext(renderer: VideoRenderer | null, videoId: string | null): PageSnapshot["playbackContext"] {
  const endpoint = renderer?.navigationEndpoint?.watchEndpoint;
  if (!videoId || renderer?.videoId !== videoId || endpoint?.videoId !== videoId
    || typeof endpoint.playlistId !== "string" || !SAFE_ID_RE.test(endpoint.playlistId) || !isWatchIndex(endpoint.index)) {
    return undefined;
  }
  const context = { playlistId: endpoint.playlistId, startIndex: endpoint.index };
  const rendererId = renderer.playlistSetVideoId;
  const endpointId = endpoint.playlistSetVideoId;
  if (rendererId != null && endpointId != null && rendererId !== endpointId) return context;
  const playlistEntryId = rendererId ?? endpointId;
  return isPlaylistEntryId(playlistEntryId) ? { ...context, playlistEntryId } : context;
}

function watchNextPlaybackContext(
  response: WatchNextResponse | undefined,
  selected: VideoRenderer | null,
  videoId: string | null,
): PageSnapshot["playbackContext"] {
  const selectedEndpoint = selected?.navigationEndpoint?.watchEndpoint;
  const entryId = selected?.playlistSetVideoId;
  if (!videoId || selected?.videoId !== videoId || !isPlaylistEntryId(entryId)
    || (selectedEndpoint?.videoId != null && selectedEndpoint.videoId !== videoId)
    || (selectedEndpoint?.playlistSetVideoId != null && selectedEndpoint.playlistSetVideoId !== entryId)) return undefined;

  // The response's currentVideoEndpoint can still describe the first track after an in-queue skip.
  const tabs = response?.contents?.singleColumnMusicWatchNextResultsRenderer?.tabbedRenderer?.watchNextTabbedResultsRenderer?.tabs;
  const panel = tabs?.[0]?.tabRenderer?.content?.musicQueueRenderer?.content?.playlistPanelRenderer;
  if (!Array.isArray(panel?.contents) || panel.contents.length > MAX_QUEUE_WINDOW) return undefined;

  let match: PageSnapshot["playbackContext"];
  for (const item of panel.contents) {
    const row = rendererOf(item);
    if (row?.videoId !== videoId) continue;
    const rowEndpoint = row.navigationEndpoint?.watchEndpoint;
    if (row.playlistSetVideoId !== entryId && rowEndpoint?.playlistSetVideoId !== entryId) continue;
    if (match) return undefined;
    match = rendererPlaybackContext(row, videoId);
    if (match?.playlistEntryId !== entryId) return undefined;
  }
  if (!match || (panel.playlistId != null && panel.playlistId !== match.playlistId)
    || (selectedEndpoint?.playlistId != null && selectedEndpoint.playlistId !== match.playlistId)
    || (selectedEndpoint?.index != null && selectedEndpoint.index !== match.startIndex)) return undefined;
  return match;
}

function playbackContextOf(
  selected: VideoRenderer | null,
  videoId: string | null,
  state: PlayerStoreState | null,
): PageSnapshot["playbackContext"] {
  const response = state?.playerPage?.playerPageWatchNextResponse;
  return rendererPlaybackContext(selected, videoId)
    ?? watchNextPlaybackContext(response, selected, videoId)
    ?? navigatedPlaybackContext(response, selected, videoId);
}

/**
 * After a navigation from another context the queue can keep selecting a row of the previous
 * queue while the player already plays the target; the response's current endpoint then names
 * the loaded context. Only used when no selected row is the playing video, because after an
 * in-queue skip that endpoint can still describe the first track.
 */
function navigatedPlaybackContext(
  response: WatchNextResponse | undefined,
  selected: VideoRenderer | null,
  videoId: string | null,
): PageSnapshot["playbackContext"] {
  const endpoint = response?.currentVideoEndpoint?.watchEndpoint;
  if (!videoId || selected?.videoId === videoId || endpoint?.videoId !== videoId
    || typeof endpoint.playlistId !== "string" || !SAFE_ID_RE.test(endpoint.playlistId) || !isWatchIndex(endpoint.index)) {
    return undefined;
  }
  const context = { playlistId: endpoint.playlistId, startIndex: endpoint.index };
  return isPlaylistEntryId(endpoint.playlistSetVideoId) ? { ...context, playlistEntryId: endpoint.playlistSetVideoId } : context;
}

function occurrenceKey(r: VideoRenderer): string {
  const index = r.navigationEndpoint?.watchEndpoint?.index;
  return `${r.videoId ?? ""}|${r.playlistSetVideoId ?? ""}|${typeof index === "number" ? index : ""}`;
}

function editableQueueKeys(queue: PageQueue | null): string[] | null {
  if (!queue || queue.start !== 0 || queue.total !== queue.items.length || queue.selectedIndex === null
    || queue.infinite || queue.hasAutoplayTail) return null;
  const keys = queue.items.map((item) => item.key);
  return keys.includes("?") || new Set(keys).size !== keys.length ? null : keys;
}

/** Autoplay suggestions only extend the queue while autoplay is on; switched off they are kept but never played. */
function hasActiveAutoplayTail(state: QueueState): boolean {
  return Array.isArray(state.automixItems) && state.automixItems.length > 0 && state.autoplay !== false;
}

/**
 * Clearing removes loaded explicit entries only. A continuation or an autoplay tail does not stop
 * it (the core reports a partial result when more may load); a radio queue refills itself, and
 * while autoplay suggestions play there is no explicit current occurrence to keep.
 */
function canClearUpcoming(state: QueueState | null, element: QueueElement): boolean {
  return state?.isInfinite === false && Array.isArray(state.automixItems) && element.queue?.autoPlaying !== true;
}

/** Occurrence keys of every loaded row, or null when one cannot be told apart from another. */
function clearableQueueKeys(state: QueueState | null): string[] | null {
  if (!Array.isArray(state?.items)) return null;
  const keys = state.items.map((raw) => {
    const r = rendererOf(raw);
    return r && trackFromRenderer(r) ? occurrenceKey(r) : "?";
  });
  return keys.includes("?") || new Set(keys).size !== keys.length ? null : keys;
}

function hasContinuation(element: QueueElement | null): boolean {
  const continuation = element?.queue?.continuation;
  return typeof continuation === "string" && continuation !== "";
}

function trackFromRenderer(r: VideoRenderer): PageTrack | null {
  if (typeof r.videoId !== "string" || !SAFE_ID_RE.test(r.videoId)) return null;
  const byline = parseByline(r.longBylineText?.runs ?? r.shortBylineText?.runs);
  return {
    videoId: r.videoId,
    title: runsText(r.title?.runs),
    artists: byline.artists,
    albumTitle: byline.albumTitle,
    durationMs: parseClockMs(runsText(r.lengthText?.runs)),
    coverUrl: pickCover(r.thumbnail?.thumbnails),
    available: r.unplayableText === undefined,
  };
}

function mapRepeat(raw: unknown): RepeatMode | null {
  if (raw === "NONE") return "off";
  if (raw === "ALL") return "all";
  if (raw === "ONE") return "one";
  return null;
}

class PageShim {
  private readonly win: Window;
  private readonly doc: Document;
  private hintSource: string | null = null;
  private hintValue: string | null = null;
  private queueAddHandler: QueueAddHandler | null = null;
  private queueHandlerRetryAt = 0;
  /** Revoked when another playback intent supersedes a pending auto-start. */
  private controlSerial = 0;

  public constructor(win: Window) {
    this.win = win;
    this.doc = win.document;
  }

  public cancelPlaybackRecovery(): number {
    this.controlSerial += 1;
    return this.controlSerial;
  }

  private api(): PlayerApi | null {
    const el = this.doc.getElementById("movie_player") as unknown as PlayerApi | null;
    return el && typeof el.getPlayerState === "function" ? el : null;
  }

  private bar(): ReduxElement | null {
    return this.doc.querySelector<ReduxElement>("ytmusic-player-bar");
  }

  private storeState(): PlayerStoreState | null {
    const bar = this.bar();
    return bar ? call(bar.getState, bar) : null;
  }

  private queueState(state = this.storeState()): QueueState | null {
    return state && isPlainObject(state.queue) ? state.queue : null;
  }

  private queueElement(state: QueueState | null): QueueElement | null {
    const element = this.doc.querySelector<QueueElement>("ytmusic-player-queue#queue");
    const store = element?.queue?.store?.store;
    return element && store && state && call(store.getState, store)?.queue === state ? element : null;
  }

  private autoplayToggle(state: QueueState | null): HTMLElement | null {
    if (typeof state?.autoplay !== "boolean") return null;
    const queue = this.queueElement(state);
    if (!queue) return null;
    // The official page renders the switch in the player page's side-panel tab header; older
    // layouts placed it inside the queue element.
    const toggle = this.doc.querySelector<HTMLElement & { checked?: boolean }>("ytmusic-player-page ytmusic-tab-renderer #automix")
      ?? queue.querySelector<HTMLElement & { checked?: boolean }>("#automix");
    if (!toggle || typeof toggle.click !== "function" || toggle.matches(":disabled, [disabled], [aria-disabled='true']")) return null;
    const ariaChecked = toggle.getAttribute("aria-checked");
    if ((typeof toggle.checked === "boolean" && toggle.checked !== state.autoplay)
      || (ariaChecked !== null && ariaChecked !== String(state.autoplay))) return null;
    return toggle;
  }

  private audioMode(): PageSnapshot["audioMode"] {
    const toggle = this.doc.querySelector("ytmusic-av-toggle[selected-queue-item-has-counterpart]");
    if (toggle?.getAttribute("selected-queue-item-has-counterpart") === "false") return null;
    const videoSelected = toggle?.getAttribute("is-video-playback-mode-selected");
    return videoSelected === "true" ? "video" : videoSelected === "false" ? "song" : null;
  }

  private button(selector: string): HTMLElement | null {
    return this.bar()?.querySelector<HTMLElement>(selector) ?? null;
  }

  private ytcfgGet(key: string): unknown {
    const ytcfg = (this.win as unknown as { ytcfg?: { get?: (k: string) => unknown } }).ytcfg;
    return typeof ytcfg?.get === "function" ? call(() => ytcfg.get!(key), ytcfg) : null;
  }

  private getQueueAddHandler(): QueueAddHandler | null {
    if (typeof this.queueAddHandler?.inst?.handleQueueAddEndpoint === "function") return this.queueAddHandler;
    this.queueAddHandler = null;
    const tag = "ytmusic-menu-service-item-renderer";
    const now = Date.now();
    if (now < this.queueHandlerRetryAt || !this.win.customElements?.get(tag)) return null;
    this.queueHandlerRetryAt = now + QUEUE_HANDLER_RETRY_MS;
    try {
      const el = this.doc.createElement(tag) as unknown as QueueAddHandler;
      if (typeof el.inst?.handleQueueAddEndpoint === "function") this.queueAddHandler = el;
    } catch {
      // A probe can precede the page's custom-element setup. Let later snapshots try again.
    }
    return this.queueAddHandler;
  }

  private navigator(): ((endpoint: unknown) => void) | null {
    const app = this.doc.querySelector("ytmusic-app") as unknown as {
      inst?: { handleNavigationEndpoint?: (endpoint: unknown) => void };
    } | null;
    const inst = app?.inst;
    const fn = inst?.handleNavigationEndpoint;
    return typeof fn === "function" ? (endpoint) => fn.call(inst, endpoint) : null;
  }

  /** SHA-256 identity hint per the shared account contract; the raw DATASYNC_ID never leaves the page. */
  private async accountHint(): Promise<string | null> {
    const raw = this.ytcfgGet("DATASYNC_ID");
    if (typeof raw !== "string" || raw === "") {
      this.hintSource = null;
      this.hintValue = null;
      return null;
    }
    if (raw !== this.hintSource) {
      this.hintSource = raw;
      this.hintValue = await computeAccountHint(raw, this.win.crypto?.subtle);
    }
    return this.hintValue;
  }

  private readQueue(state: QueueState | null): PageQueue | null {
    if (!state || !Array.isArray(state.items)) return null;
    const total = state.items.length;
    const selected =
      typeof state.selectedItemIndex === "number" && Number.isSafeInteger(state.selectedItemIndex) && state.selectedItemIndex >= 0 && state.selectedItemIndex < total
        ? state.selectedItemIndex
        : null;
    const start = Math.max(0, Math.min((selected ?? 0) - QUEUE_WINDOW_BEFORE_CURRENT, total - MAX_QUEUE_WINDOW));
    const items: PageQueueItem[] = [];
    for (const raw of state.items.slice(start, start + MAX_QUEUE_WINDOW)) {
      const r = rendererOf(raw);
      const track = r ? trackFromRenderer(r) : null;
      // Keep positions aligned with the page queue even for items we cannot parse.
      items.push(
        track && r
          ? { ...track, key: occurrenceKey(r) }
          : { videoId: "unknown", title: "", artists: [], albumTitle: null, durationMs: null, coverUrl: null, available: false, key: "?" }
      );
    }
    return {
      start,
      total,
      items,
      selectedIndex: selected,
      infinite: state.isInfinite === true,
      hasAutoplayTail: hasActiveAutoplayTail(state),
      continuation: hasContinuation(this.queueElement(state)),
    };
  }

  public async snapshot(): Promise<PageSnapshot> {
    const api = this.api();
    const store = this.storeState();
    const state = this.queueState(store);
    const videoData = api ? call(api.getVideoData, api) : null;
    const videoId = typeof videoData?.video_id === "string" && SAFE_ID_RE.test(videoData.video_id) ? videoData.video_id : null;
    const details = api ? call(api.getPlayerResponse, api)?.videoDetails ?? null : null;
    const detailsMatch = details?.videoId === videoId && videoId !== null;

    const selectedRenderer = selectedRendererOf(state);
    // The official queue can keep selecting a row after the player moved on. A selection the
    // playing video does not corroborate names no current occurrence; the window stays put.
    const pageQueue = this.readQueue(state);
    const queue = pageQueue && pageQueue.selectedIndex !== null && (videoId === null || selectedRenderer?.videoId !== videoId)
      ? { ...pageQueue, selectedIndex: null }
      : pageQueue;
    const selectedTrack = selectedRenderer ? trackFromRenderer(selectedRenderer) : null;

    let track: PageTrack | null = null;
    if (videoId && selectedTrack?.videoId === videoId) {
      track = selectedTrack;
    } else if (videoId && detailsMatch && details) {
      track = {
        videoId,
        title: details.title ?? "",
        artists: details.author ? [details.author] : [],
        albumTitle: null,
        durationMs: null,
        coverUrl: pickCover(details.thumbnail?.thumbnails),
        available: true,
      };
    }

    const lengthSec = detailsMatch ? Number(details?.lengthSeconds) : Number.NaN;
    const durationMs =
      Number.isFinite(lengthSec) && lengthSec > 0 ? Math.round(lengthSec * 1000) : track?.durationMs ?? null;
    if (track && durationMs !== null) track = { ...track, durationMs };

    const currentSec = api ? finiteOrNull(call(api.getCurrentTime, api)) : null;
    const cpn = api ? call(api.getCpn, api) : null;
    const instanceKey = typeof cpn === "string" && cpn !== "" ? cpn : null;
    const playerState = api ? finiteOrNull(call(api.getPlayerState, api)) : null;
    const activeDiagnostics = api && videoId && instanceKey && currentSec !== null && currentSec > 0
      && (playerState === 1 || playerState === 2) && !this.doc.getElementById("movie_player")?.classList.contains("ad-showing");
    const stats = activeDiagnostics ? call(api.getStatsForNerds, api) : null;
    const codecs = isPlainObject(stats) ? stats.codecs : null;
    const quality = activeDiagnostics ? call(api.getPlaybackQuality, api) : null;
    const loggedIn = this.ytcfgGet("LOGGED_IN") === true;
    const shuffle = typeof state?.shuffleEnabled === "boolean" ? state.shuffleEnabled : null;
    const repeat = mapRepeat(state?.repeatMode);
    const autoplay = typeof state?.autoplay === "boolean" ? state.autoplay : null;

    const queueElement = this.queueElement(state);
    const editQueue = videoId !== null && selectedRenderer?.videoId === videoId
      && instanceKey !== null && editableQueueKeys(queue) !== null
      && typeof queueElement?.dispatch === "function";
    const controls: PageControls = {
      playPause: typeof api?.playVideo === "function" && typeof api?.pauseVideo === "function",
      seek: typeof api?.seekTo === "function" && typeof api?.getCurrentTime === "function",
      next: this.button(".next-button") !== null || typeof api?.nextVideo === "function",
      previous: this.button(".previous-button") !== null || typeof api?.previousVideo === "function",
      volume: typeof api?.setVolume === "function" && typeof api?.getVolume === "function",
      shuffle: this.button(".shuffle") !== null && shuffle !== null,
      repeat: this.button(".repeat") !== null && repeat !== null,
      enqueue: queue !== null && this.getQueueAddHandler() !== null,
      editQueue,
      clearUpcoming: videoId !== null && selectedRenderer?.videoId === videoId && instanceKey !== null
        && queue?.selectedIndex != null && typeof queueElement?.dispatch === "function"
        && clearableQueueKeys(state) !== null && canClearUpcoming(state, queueElement),
      setAutoplay: autoplay !== null && this.autoplayToggle(state) !== null,
      navigate: this.navigator() !== null,
    };

    return {
      hasPlayerApi: api !== null,
      instanceKey,
      videoId,
      track,
      playbackContext: playbackContextOf(selectedRenderer, videoId, store),
      positionMs: currentSec !== null && currentSec >= 0 ? Math.round(currentSec * 1000) : null,
      durationMs,
      loadedFraction: api ? finiteOrNull(call(api.getVideoLoadedFraction, api)) : null,
      playerState,
      volume0To100: api ? finiteOrNull(call(api.getVolume, api)) : null,
      muted: api && typeof api.isMuted === "function" ? Boolean(call(api.isMuted, api)) : null,
      playbackRate: api ? finiteOrNull(call(api.getPlaybackRate, api)) : null,
      shuffle,
      repeat,
      autoplay,
      codec: typeof codecs === "string" && codecs.length <= 128
        ? AUDIO_CODEC_STATS_RE.exec(codecs)?.[1]?.toLowerCase() ?? null : null,
      qualityLabel: typeof quality === "string" && QUALITY_LABEL_RE.test(quality) ? quality : null,
      audioMode: activeDiagnostics && selectedRenderer?.videoId === videoId ? this.audioMode() : null,
      loggedIn,
      accountHint: loggedIn ? await this.accountHint() : null,
      queue,
      controls,
    };
  }

  private clickControl(selector: string, fallback: (() => void) | undefined, api: PlayerApi | null): ShimResult {
    const btn = this.button(selector);
    if (btn && !btn.hasAttribute("disabled") && btn.getAttribute("aria-disabled") !== "true") {
      btn.click();
      return { ok: true };
    }
    if (typeof fallback === "function") {
      fallback.call(api);
      return { ok: true };
    }
    return { ok: false, error: "UNAVAILABLE", message: `${selector} unavailable` };
  }

  /**
   * Starts the loaded track if the page left it unstarted. YouTube Music sometimes loads the
   * next track (or a navigation target) at state -1 and never starts it; playTarget and a skip
   * from playback both mean "keep playing". Resolves once playing, started, superseded by a
   * newer control op, or at the deadline.
   */
  private async keepPlaying(
    serial: number,
    loaded: (p: PlayerApi, nonce: string | null) => boolean,
    watchMs: number
  ): Promise<void> {
    const deadline = Date.now() + watchMs;
    let idleSince: number | null = null;
    let target: { api: PlayerApi; nonce: string | null } | null = null;
    while (Date.now() < deadline && serial === this.controlSerial) {
      const p = this.api();
      const nonce = p ? call(p.getCpn, p) : null;
      const matches = p !== null && loaded(p, nonce);
      // A further page load after we first matched belongs to a different playback intent.
      if (target && (!matches || p !== target.api || nonce !== target.nonce)) return;
      if (p && matches) {
        target ??= { api: p, nonce };
        const state = call(p.getPlayerState, p);
        if (state === 1 || state === 0) return;
        if (state === -1 || state === 5 || state === 2) {
          idleSince ??= Date.now();
          if (Date.now() - idleSince >= STUCK_UNSTARTED_MS) {
            call(p.playVideo, p);
            return;
          }
        } else {
          idleSince = null;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }

  private skip(serial: number, selector: string, fallback: (() => void) | undefined, api: PlayerApi | null): ShimResult {
    const state = api ? call(api.getPlayerState, api) : null;
    const loadBefore = api ? call(api.getCpn, api) : null;
    const result = this.clickControl(selector, fallback, api);
    if (result.ok && (state === 1 || state === 3)) {
      // A restart within the track keeps the load nonce, so only a new load is watched.
      void this.keepPlaying(serial, (_, nonce) => nonce !== null && nonce !== "" && nonce !== loadBefore, SKIP_START_WATCH_MS);
    }
    return result;
  }

  public async execute(op: PageShimOp, args: Record<string, unknown>): Promise<ShimResult> {
    if (op === "probe") {
      return { ok: true, data: await this.snapshot() };
    }
    const serial = this.cancelPlaybackRecovery();
    const api = this.api();
    const unavailable = (what: string): ShimResult => ({ ok: false, error: "UNAVAILABLE", message: `${what} unavailable` });
    const invalid = (what: string): ShimResult => ({ ok: false, error: "INVALID_ARGS", message: what });

    switch (op) {
      case "play":
        if (typeof api?.playVideo !== "function") return unavailable("playVideo");
        api.playVideo();
        return { ok: true };
      case "pause":
        if (typeof api?.pauseVideo !== "function") return unavailable("pauseVideo");
        api.pauseVideo();
        return { ok: true };
      case "seek": {
        const sec = args.positionSec;
        if (typeof sec !== "number" || !Number.isFinite(sec) || sec < 0) return invalid("positionSec");
        if (typeof api?.seekTo !== "function") return unavailable("seekTo");
        api.seekTo(sec, true);
        return { ok: true };
      }
      case "next":
        return this.skip(serial, ".next-button", api?.nextVideo, api);
      case "previous":
        return this.skip(serial, ".previous-button", api?.previousVideo, api);
      case "setVolume": {
        const v = args.volume01;
        if (typeof v !== "number" || !Number.isFinite(v) || v < 0 || v > 1) return invalid("volume01");
        if (typeof api?.setVolume !== "function") return unavailable("setVolume");
        api.setVolume(Math.round(v * 100));
        return { ok: true };
      }
      case "setMuted": {
        if (typeof args.muted !== "boolean") return invalid("muted");
        const fn = args.muted ? api?.mute : api?.unMute;
        if (typeof fn !== "function") return unavailable("mute/unMute");
        fn.call(api);
        return { ok: true };
      }
      case "setShuffle": {
        if (typeof args.shuffle !== "boolean") return invalid("shuffle");
        const btn = this.button(".shuffle");
        const current = this.queueState()?.shuffleEnabled;
        if (!btn || typeof current !== "boolean") return unavailable("shuffle");
        if (current !== args.shuffle) btn.click();
        return this.queueState()?.shuffleEnabled === args.shuffle
          ? { ok: true }
          : { ok: false, error: "NO_EFFECT", message: "shuffle state did not change" };
      }
      case "setRepeat": {
        const target = args.repeat;
        if (target !== "off" && target !== "all" && target !== "one") return invalid("repeat");
        const btn = this.button(".repeat");
        if (!btn || mapRepeat(this.queueState()?.repeatMode) === null) return unavailable("repeat");
        // The page only offers a NONE -> ALL -> ONE cycle.
        for (let i = 0; i < 3 && mapRepeat(this.queueState()?.repeatMode) !== target; i++) btn.click();
        return mapRepeat(this.queueState()?.repeatMode) === target
          ? { ok: true }
          : { ok: false, error: "NO_EFFECT", message: "repeat state did not reach target" };
      }
      case "setAutoplay": {
        if (typeof args.autoplay !== "boolean") return invalid("autoplay");
        const state = this.queueState();
        const toggle = this.autoplayToggle(state);
        if (!toggle || typeof state?.autoplay !== "boolean") return unavailable("queue autoplay");
        if (state.autoplay !== args.autoplay) toggle.click();
        return { ok: true };
      }
      case "navigate": {
        const { videoId, playlistId, index, requireContext, playlistEntryId } = args;
        if (typeof videoId !== "string" || !SAFE_ID_RE.test(videoId)) return invalid("videoId");
        if (playlistId !== undefined && (typeof playlistId !== "string" || !SAFE_ID_RE.test(playlistId))) return invalid("playlistId");
        if (index !== undefined && !isWatchIndex(index)) return invalid("index");
        if (requireContext === true && (playlistId === undefined || index === undefined)) return invalid("playlistId/index");
        if (requireContext === true && playlistEntryId !== undefined && !isPlaylistEntryId(playlistEntryId)) return invalid("playlistEntryId");
        const navigate = this.navigator();
        if (!navigate) return unavailable("in-page navigation");
        navigate({
          watchEndpoint: {
            videoId,
            ...(playlistId !== undefined ? { playlistId } : {}),
            ...(index !== undefined ? { index } : {}),
            ...(requireContext === true && playlistEntryId !== undefined ? { playlistSetVideoId: playlistEntryId } : {}),
          },
        });
        // A collection may contain the already-loaded video at a different position or from a different list.
        await this.keepPlaying(serial, (p) => {
          if (call(p.getVideoData, p)?.video_id !== videoId) return false;
          if (requireContext !== true) return true;
          const store = this.storeState();
          const context = playbackContextOf(selectedRendererOf(this.queueState(store)), videoId, store);
          return context !== undefined && context.playlistId === playlistId && context.startIndex === index
            && (playlistEntryId === undefined || context.playlistEntryId === playlistEntryId);
        }, NAVIGATE_SETTLE_MS);
        return { ok: true };
      }
      case "selectQueueItem": {
        const { index, key } = args;
        if (typeof index !== "number" || !Number.isSafeInteger(index) || index < 0 || typeof key !== "string") {
          return invalid("index/key");
        }
        const items = this.queueState()?.items;
        const r = rendererOf(items?.[index]);
        if (!items || !r || occurrenceKey(r) !== key) {
          return { ok: false, error: "STALE", message: "queue item moved or was replaced" };
        }
        const contents = this.doc.querySelector("ytmusic-player-queue #contents");
        const row = contents?.children[index] ?? null;
        const item = row?.matches("ytmusic-player-queue-item") ? row : row?.querySelector("ytmusic-player-queue-item") ?? null;
        const rendered = (item as unknown as { data?: VideoRenderer } | null)?.data;
        if (!item || !rendered || occurrenceKey(rendered) !== key) {
          return { ok: false, error: "STALE", message: "queue DOM does not match queue state" };
        }
        if (items.some((other, i) => {
          const renderer = rendererOf(other);
          return i !== index && renderer !== null && occurrenceKey(renderer) === key;
        })) {
          return { ok: false, error: "STALE", message: "queue occurrence is ambiguous" };
        }
        (item.querySelector<HTMLElement>("ytmusic-play-button-renderer") ?? (item as HTMLElement)).click();
        return { ok: true };
      }
      case "clearUpcoming": {
        const { index, expectedKeys, expectedSelectedIndex, expectedInstanceKey } = args;
        const state = this.queueState();
        const queue = this.readQueue(state);
        const keys = clearableQueueKeys(state);
        const element = this.queueElement(state);
        const dispatch = element?.dispatch;
        if (typeof dispatch !== "function" || !element || !keys || !queue || !canClearUpcoming(state, element)) {
          return unavailable("clearUpcoming on this queue");
        }
        if (typeof index !== "number" || !Number.isSafeInteger(index) || !Array.isArray(expectedKeys)
          || typeof expectedInstanceKey !== "string") {
          return invalid("clearUpcoming requires the current occurrence, order and playback instance");
        }
        // The bridge sees the reported window; it must match the same rows of the loaded queue.
        const selected = queue.selectedIndex;
        const activeVideo = selected === null ? null : queue.items[selected - queue.start]?.videoId;
        if (expectedKeys.length !== queue.items.length
          || !expectedKeys.every((key, i) => key === keys[queue.start + i])
          || selected !== expectedSelectedIndex || !api || call(api.getCpn, api) !== expectedInstanceKey
          || selected === null || activeVideo !== call(api.getVideoData, api)?.video_id) {
          return { ok: false, error: "STALE", message: "queue order or active playback changed" };
        }
        if (index !== selected) return invalid("clearUpcoming requires the active occurrence");
        // At most the nearest 50 go in one clear; the core reports the rest as still following.
        const lastRemoved = Math.min(keys.length - 1, selected + MAX_CLEAR_UPCOMING);
        // Remove backwards so every remaining index stays valid. Before each further removal
        // the store must hold exactly the rows that are left, behind the same playback.
        for (let tail = lastRemoved; tail > selected; tail--) {
          if (tail !== lastRemoved) {
            const gone = lastRemoved - tail;
            const next = this.queueState();
            if (next?.selectedItemIndex !== selected || !Array.isArray(next.items)
              || next.items.length !== keys.length - gone || !canClearUpcoming(next, element)) break;
            const sameStore = this.queueElement(next) === element && element.dispatch === dispatch;
            const samePlayback = this.controlSerial === serial && this.api() === api
              && call(api.getCpn, api) === expectedInstanceKey && call(api.getVideoData, api)?.video_id === activeVideo;
            const expectedOrder = next.items.every((raw, i) => {
              const renderer = rendererOf(raw);
              return renderer !== null && occurrenceKey(renderer) === keys[i > tail ? i + gone : i];
            });
            if (!sameStore || !samePlayback || !expectedOrder) break;
          }
          try {
            dispatch.call(element, { type: "REMOVE_ITEM", payload: tail });
          } catch {
            // A reducer can apply before a subscriber throws. Stop the sequence and let core read back.
            break;
          }
        }
        return { ok: true };
      }
      case "removeQueueItem":
      case "moveQueueItem": {
        const { index, beforeIndex, expectedKeys, expectedSelectedIndex, expectedInstanceKey } = args;
        const state = this.queueState();
        const queue = this.readQueue(state);
        const keys = editableQueueKeys(queue);
        const element = this.queueElement(state);
        const dispatch = element?.dispatch;
        if (typeof dispatch !== "function" || !element || !keys || !queue) return unavailable("finite exact queue edit");
        if (typeof index !== "number" || !Number.isSafeInteger(index) || index < 0 || index >= keys.length
          || !Array.isArray(expectedKeys) || typeof expectedInstanceKey !== "string") {
          return invalid("queue edit requires the exact occurrence, order and playback instance");
        }
        const selected = queue.selectedIndex;
        const activeVideo = selected === null ? null : queue.items[selected]?.videoId;
        if (expectedKeys.length !== keys.length || !keys.every((key, i) => key === expectedKeys[i])
          || selected !== expectedSelectedIndex || !api || call(api.getCpn, api) !== expectedInstanceKey
          || selected === null || activeVideo !== call(api.getVideoData, api)?.video_id) {
          return { ok: false, error: "STALE", message: "queue order or active playback changed" };
        }
        if (index === selected) return invalid("cannot edit the actively playing occurrence");
        let action: QueueEditAction = { type: "REMOVE_ITEM", payload: index };
        if (op === "moveQueueItem") {
          if (beforeIndex !== null && (typeof beforeIndex !== "number" || !Number.isSafeInteger(beforeIndex)
            || beforeIndex < 0 || beforeIndex >= keys.length || beforeIndex === index)) {
            return invalid("move requires a distinct queue anchor or the finite tail");
          }
          const destination = beforeIndex === null ? keys.length - 1 : beforeIndex - Number(index < beforeIndex);
          if (destination === index) return invalid("queue occurrence is already at the requested position");
          action = { type: "MOVE_ITEM", payload: { fromIndex: index, toIndex: destination } };
        }
        try {
          dispatch.call(element, action);
        } catch {
          // A store subscriber can throw after the reducer ran. The core must settle by readback.
        }
        return { ok: true };
      }
      case "enqueue": {
        const { videoId, position } = args;
        if (typeof videoId !== "string" || !SAFE_ID_RE.test(videoId)) return invalid("videoId");
        if (position !== "next" && position !== "last") return invalid("position");
        const state = this.queueState();
        const handler = this.getQueueAddHandler();
        if (!state || !handler?.inst?.handleQueueAddEndpoint) return unavailable("queue add");
        if (position === "last" && state.isInfinite === true) {
          return { ok: false, error: "INFINITE_QUEUE", message: "cannot append to an endless radio queue" };
        }
        // Same handler YouTube Music's own "Play next" / "Add to queue" menu items use.
        const pending = handler.inst.handleQueueAddEndpoint({
          queueAddEndpoint: {
            queueTarget: { videoId, onEmptyQueue: { watchEndpoint: { videoId } } },
            queueInsertPosition: position === "next" ? "INSERT_AFTER_CURRENT_VIDEO" : "INSERT_AT_END",
            commands: [],
          },
        });
        if (pending && typeof (pending as Promise<unknown>).then === "function") {
          await Promise.race([
            (pending as Promise<unknown>).catch(() => undefined),
            new Promise((resolve) => setTimeout(resolve, ENQUEUE_TIMEOUT_MS)),
          ]);
        }
        return { ok: true };
      }
    }
    return invalid("op");
  }
}

/** `acct:` + first 32 hex chars of SHA-256("ytm-glass-account-v1\n" + datasyncId). */
export async function computeAccountHint(
  datasyncId: string,
  subtle: SubtleCrypto | undefined
): Promise<string | null> {
  if (!subtle) return null;
  try {
    const digest = await subtle.digest("SHA-256", new TextEncoder().encode(ACCOUNT_HINT_DOMAIN + datasyncId));
    const hex = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
    return `acct:${hex.slice(0, 32)}`;
  } catch {
    return null;
  }
}

/**
 * Installs the page shim on the music.youtube.com top frame. Idempotent: a previous
 * installation on the same window is removed first.
 */
export function installPageShim(win: Window = window): () => void {
  const host = win as unknown as Record<symbol, (() => void) | undefined>;
  host[SHIM_CLEANUP_SYMBOL]?.();
  delete host[SHIM_CLEANUP_SYMBOL];
  if (!isTrustedMusicTopFrame(win)) {
    return () => {};
  }
  const shim = new PageShim(win);
  let active = true;

  const onMessage = (event: MessageEvent): void => {
    if (!isTrustedMusicTopFrame(win) || event.source !== win || event.origin !== MUSIC_ORIGIN) return;
    const raw = event.data;
    if (!isPlainObject(raw) || raw.channel !== PAGE_SHIM_CHANNEL || raw.direction !== "to-page") return;
    if (typeof raw.requestId !== "string" || raw.requestId === "") return;
    if (typeof raw.op !== "string" || !ALLOWED_OPS.has(raw.op)) return;
    const requestId = raw.requestId;
    const args = isPlainObject(raw.args) ? raw.args : {};
    void shim
      .execute(raw.op as PageShimOp, args)
      // A throw can follow a partial page change, so it is not reported as a clean refusal.
      .catch((): ShimResult => ({ ok: false, error: "PAGE_CALL_FAILED", message: "page call failed" }))
      .then((result) => {
        if (!active) return;
        const reply: PageShimResponse = { channel: PAGE_SHIM_CHANNEL, direction: "from-page", requestId, ...result };
        win.postMessage(reply, MUSIC_ORIGIN);
      });
  };

  const onPlaybackClick = (event: MouseEvent): void => {
    if (!event.isTrusted) return;
    const Element = win.document.defaultView?.Element;
    if (Element && event.composedPath().some((part) => part instanceof Element && part.matches(PLAYBACK_INPUT_SELECTOR))) {
      shim.cancelPlaybackRecovery();
    }
  };
  const onPlaybackKey = (event: KeyboardEvent): void => {
    if (event.isTrusted && PLAYBACK_MEDIA_KEYS.has(event.key)) shim.cancelPlaybackRecovery();
  };
  win.addEventListener("message", onMessage);
  win.addEventListener("click", onPlaybackClick, true);
  win.addEventListener("keydown", onPlaybackKey, true);
  const cleanup = (): void => {
    active = false;
    shim.cancelPlaybackRecovery();
    win.removeEventListener("message", onMessage);
    win.removeEventListener("click", onPlaybackClick, true);
    win.removeEventListener("keydown", onPlaybackKey, true);
  };
  host[SHIM_CLEANUP_SYMBOL] = cleanup;
  return cleanup;
}

if (typeof window !== "undefined") {
  installPageShim(window);
}
