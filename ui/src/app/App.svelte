<script lang="ts">
  import {
    createSyntheticTracks,
    type AgentOperationSummary,
    type AppSnapshot,
    type CapabilitySet,
    type CapabilitySupport,
    type CommandReceipt,
    type EnqueuePosition,
    type FakeScenario,
    type LyricsResult,
    type MutationRequest,
    type PlayerCommandPayload,
    type PlayerOp,
    type PlaylistSummary,
    type QueryError,
    type Track,
  } from "@ytmcp/protocol";
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { SvelteMap } from "svelte/reactivity";
  import { commandGate, playPauseGate, writeGate } from "../lib/capabilities.js";
  import {
    CatalogStore,
    albumPlaybackContext,
    catalogAccountKey,
    type CatalogPage,
    type CatalogRoute,
  } from "../lib/catalog/catalog.svelte.js";
  import { isSignedOut, resolvePageView, type PageView } from "../lib/catalog/page-state.js";
  import { AmbientStore, setAmbient } from "../lib/ambient/ambient.svelte.js";
  import AccessibleDialog from "../lib/components/AccessibleDialog.svelte";
  import AgentActivity from "../lib/components/AgentActivity.svelte";
  import AlbumView from "../lib/components/AlbumView.svelte";
  import AmbientLayer from "../lib/components/AmbientLayer.svelte";
  import ArtistView from "../lib/components/ArtistView.svelte";
  import HomeView from "../lib/components/HomeView.svelte";
  import LibraryView from "../lib/components/LibraryView.svelte";
  import LyricsView from "../lib/components/LyricsView.svelte";
  import NowPlayingView from "../lib/components/NowPlayingView.svelte";
  import PlayerBar from "../lib/components/PlayerBar.svelte";
  import QueueView from "../lib/components/QueueView.svelte";
  import SearchView from "../lib/components/SearchView.svelte";
  import { recentSearches } from "../lib/recent-searches.svelte.js";
  import SettingsView from "../lib/components/SettingsView.svelte";
  import Sidebar from "../lib/components/Sidebar.svelte";
  import { SidebarMotion } from "../lib/components/sidebar-motion.svelte.js";
  import ToastHost from "../lib/components/ToastHost.svelte";
  import TrackActions from "../lib/components/TrackActions.svelte";
  import TrackCollectionView from "../lib/components/TrackCollectionView.svelte";
  import { ToastStore, currentProblem, type ProblemKind, type Toast } from "../lib/feedback/toasts.svelte.js";
  import {
    operationKey,
    pendingAccountHandoff,
    settledAgentChange,
  } from "../lib/feedback/agent-news.js";
  import { getLocale, setLocale, t, type NavView } from "../lib/i18n/index.svelte.js";
  import { queryErrorDetail } from "../lib/i18n/query-errors.js";
  import { createUiClient, type UiClientState, type YtmUiClient } from "../lib/ipc/client.js";
  import { stepUserOffsetMs } from "../lib/lyrics.js";
  import { shouldRunDecorativeAnimation } from "../lib/motion/preferences.js";
  import { SeekControl } from "../lib/seek-control.svelte.js";
  import { ReturnFrames, setReturnFrames, type ListPosition } from "../lib/return-frame.js";
  import { resolveShortcut } from "../lib/shortcuts.js";
  import { accessibleTrackAccent } from "../lib/theme/accent.js";

  /** One seek preview for the player bar and Now Playing, so both follow the same drag. */
  const seek = new SeekControl();

  interface Props {
    client?: YtmUiClient;
    initialView?: NavView;
  }

  let { client: providedClient, initialView = "home" }: Props = $props();

  type Route =
    | { readonly view: NavView }
    | { readonly view: "playlist" | "album" | "artist"; readonly id: string };

  /** Pages showing private account data: never fetched or shown while signed out. */
  const ACCOUNT_VIEWS: ReadonlySet<Route["view"]> = new Set(["library", "playlist", "liked", "recent"]);

  /** The capability that decides whether a catalog page may be requested at all. */
  function routeCapability(view: Route["view"], c: CapabilitySet | null | undefined): CapabilitySupport | undefined {
    switch (view) {
      case "search":
        return c?.search;
      case "library":
        return c?.libraryPlaylists;
      case "playlist":
        return c?.playlistTracks;
      case "album":
        return c?.albumDetail;
      case "artist":
        return c?.artistDetail;
      case "liked":
        return c?.likedSongs;
      default:
        return undefined;
    }
  }

  const client: YtmUiClient = untrack(() => providedClient ?? createUiClient());
  const store = new CatalogStore(client);
  const MAX_HISTORY = 50;
  const frames = new ReturnFrames();
  setReturnFrames(frames);
  /** Pages report their main cover here; the shell's ambient light follows it. */
  const ambient = new AmbientStore();
  setAmbient(ambient);
  /** The one transient toast; account and connection problems are derived below. */
  const toasts = new ToastStore();

  let clientState = $state<UiClientState>(client.getState());
  let route = $state<Route>({ view: untrack(() => initialView) });
  let history = $state.raw<Route[]>([]);
  /** Collapse state of the sidebar and its transition (the shell's data-sidebar and data-sidebar-motion). */
  const sidebar = new SidebarMotion();
  let activeScenario = $state<FakeScenario>("normal");
  let diagnosticsOpen = $state(false);
  let lastContentError = $state.raw<QueryError | null>(null);
  let imeComposing = $state(false);
  let documentHidden = $state(false);
  let prefersReducedMotion = $state(false);
  let prefersLight = $state(false);
  let interpolatedPositionMs = $state(0);
  let mainEl = $state<HTMLElement | null>(null);

  /** Where lyrics was opened from with the toggle, and how that page looked when it was left. */
  interface LyricsReturn {
    readonly identity: string;
    readonly mainScrollTop: number;
    readonly list: ListPosition | null;
  }
  let lyricsReturn: LyricsReturn | null = null;
  /** The Queue page's Played section, kept open across leaving the page so its scroll position still fits. */
  let queuePlayedOpen = $state(false);

  let lyrics = $state<LyricsResult | null>(null);
  let lyricsLoading = $state(false);
  let lyricsSeq = 0;
  let requestedLyricsKey: string | null = null;

  let trackActionsSequence = 0;
  let trackActionsRequest = $state<{ key: number; track: Track; startAt?: "pick" } | null>(null);
  let trackActionsOpen = $state(false);
  const actionsFor = $derived(trackActionsOpen ? trackActionsRequest : null);
  /** The actions dialog's playlist picker has asked for the library (it shows the library's state). */
  let pickerRequested = $state(false);
  const pendingWrites = new SvelteMap<string, symbol>();
  const likeOverrides = new SvelteMap<string, boolean>();
  const invalidatedRatings = new SvelteMap<string, number>();
  let ratingSequence = 0;

  const snapshot = $derived(clientState.snapshot);
  const hasSnapshot = $derived(snapshot !== null);
  const auth = $derived(snapshot?.authState ?? null);
  const caps = $derived(snapshot?.capabilities ?? null);
  const network = $derived(snapshot?.networkState ?? "unknown");
  const mockMode = $derived(snapshot?.mockMode ?? false);
  const currentTrack = $derived(snapshot?.player.currentTrack ?? null);
  const currentVideoId = $derived(currentTrack?.videoId ?? null);
  const connection = $derived(snapshot?.player.connection ?? null);
  const accountKey = $derived(catalogAccountKey(auth));

  const settings = $derived(snapshot?.settings ?? null);
  const motionReduced = $derived(settings?.reduceMotion === true || prefersReducedMotion);
  const colorScheme = $derived(
    settings?.theme === "light" || (settings?.theme === "system" && prefersLight) ? "light" : "dark"
  );
  const decorativeMotion = $derived(
    shouldRunDecorativeAnimation({
      reduceMotionSetting: settings?.reduceMotion === true,
      prefersReducedMotion,
      documentHidden,
      playbackStatus: snapshot?.player.playback ?? "idle",
    })
  );
  /** The ambient light breathes only while a track is actually playing: not while buffering, paused or hidden. */
  const ambientBreathing = $derived(snapshot?.player.playback === "playing" && !documentHidden && !motionReduced);
  const trackAccent = $derived(accessibleTrackAccent(currentTrack?.cover?.accentOklch, colorScheme));

  const playGate = $derived(playPauseGate(caps?.playPause, connection, snapshot?.player.playback, currentTrack !== null));
  const seekGate = $derived(commandGate(caps?.seek, connection));
  const nextGate = $derived(commandGate(caps?.next, connection));
  const prevGate = $derived(commandGate(caps?.previous, connection));
  const enqueueGate = $derived(commandGate(caps?.queueEnqueue, connection));
  const likeGate = $derived(writeGate(caps?.likeMutation, auth));
  // Track list rows: a like toggle only while likes can be written, and the playing row's indicator.
  const rowLike = $derived(
    likeGate.enabled
      ? {
          liked: (track: Track) => likedState(track),
          pending: (track: Track) => pendingWrites.has(`setLike:${track.videoId}`),
          toggle: (track: Track) => setLike(track, likedState(track) !== true),
        }
      : undefined
  );
  const rowPlayback = $derived({ playback: snapshot?.player.playback ?? null, connection, motion: decorativeMotion });
  const playlistGate = $derived(writeGate(caps?.playlistAddMutation, auth));
  const canOpenAlbum = $derived(caps?.albumDetail !== "unsupported");
  const canOpenArtist = $derived(caps?.artistDetail !== "unsupported");

  const boolLabel = (v: boolean | null | undefined) => (v == null ? t.common.unknown : v ? t.common.yes : t.common.no);

  const navView = $derived<NavView | null>(
    route.view === "playlist" ? "library" : route.view === "album" || route.view === "artist" ? null : route.view
  );

  /** `expectedKey` set: content for another id (previous playlist/album) counts as not loaded yet. */
  function pageView(
    view: Route["view"],
    page: Pick<CatalogPage<unknown, unknown>, "key" | "status" | "error" | "hasData" | "stale">,
    isEmpty: boolean,
    expectedKey?: string
  ): PageView {
    const otherKey = expectedKey !== undefined && page.key !== expectedKey;
    return resolvePageView({
      status: otherKey ? "loading" : page.status,
      error: otherKey ? null : page.error,
      hasData: otherKey ? false : page.hasData,
      isEmpty,
      stale: page.stale,
      capability: routeCapability(view, caps),
      authState: auth?.state,
      network,
      accountScoped: ACCOUNT_VIEWS.has(view),
    });
  }

  const routeId = $derived("id" in route ? route.id : undefined);
  const homeView = $derived(
    pageView(
      "home",
      store.home,
      (store.home.detail?.recentTracks?.length ?? 0) === 0 && (store.home.detail?.pinnedPlaylists?.length ?? 0) === 0
    )
  );
  const searchView = $derived(
    pageView(
      "search",
      store.search,
      store.search.items.length === 0 &&
        (store.search.detail?.albums?.length ?? 0) === 0 &&
        (store.search.detail?.artists?.length ?? 0) === 0 &&
        (store.search.detail?.playlists?.length ?? 0) === 0
    )
  );
  const searchBlocked = $derived(searchView.kind === "unsupported" || searchView.kind === "authExpired");
  const libraryView = $derived(pageView("library", store.library, store.library.items.length === 0));
  const playlistView = $derived(pageView("playlist", store.playlist, store.playlist.items.length === 0, routeId));
  const albumView = $derived(pageView("album", store.album, store.album.items.length === 0, routeId));
  const artistView = $derived(
    pageView(
      "artist",
      store.artist,
      store.artist.items.length === 0 && (store.artist.detail?.albums?.length ?? 0) === 0,
      routeId
    )
  );
  const likedView = $derived(pageView("liked", store.liked, store.liked.items.length === 0));
  const recentView = $derived(pageView("recent", store.recent, store.recent.items.length === 0));

  /** True when the route's data must not be requested (unsupported, or private while signed out). */
  function routeBlocked(r: Route): boolean {
    return (
      routeCapability(r.view, snapshot?.capabilities) === "unsupported" ||
      (ACCOUNT_VIEWS.has(r.view) && isSignedOut(snapshot?.authState.state))
    );
  }

  /** Fetches what the route shows; repeated calls reuse loaded pages unless forced. */
  function loadRoute(r: Route, force = false) {
    if (routeBlocked(r)) return;
    switch (r.view) {
      case "home":
        void store.loadHome(force);
        break;
      case "search":
        if (store.search.key && (force || store.search.status === "idle")) {
          void store.runSearch(store.search.key);
        }
        break;
      case "library":
        void store.loadLibrary(force);
        break;
      case "playlist":
        void store.openPlaylist(r.id, force);
        break;
      case "album":
        void store.openAlbum(r.id, force);
        break;
      case "artist":
        void store.openArtist(r.id, force);
        break;
      case "liked":
        void store.loadLiked(force);
        break;
      case "recent":
        void store.loadRecent(force);
        break;
      default:
        break;
    }
  }

  const routeCap = $derived(routeCapability(route.view, caps));

  // Account identity change (switch, logout, expiry) clears account data; then load the route.
  // The route's capability is tracked too, so a page that was blocked loads once it is supported.
  $effect(() => {
    if (!hasSnapshot) return;
    const key = accountKey;
    const r = route;
    void routeCap;
    untrack(() => {
      // Recent searches belong to one account: wiped as soon as a sign-out or another account is seen.
      recentSearches.sync(key);
      if (store.syncAccount(key)) {
        likeOverrides.clear();
        invalidatedRatings.clear();
        pendingWrites.clear();
        sentEnqueues.clear();
        toasts.dismiss();
        lastContentError = null;
        closeTrackActions();
        lyricsReturn = null;
        frames.clear();
      }
      loadRoute(r);
    });
  });

  function sameRoute(a: Route, b: Route): boolean {
    return a.view === b.view && ("id" in a ? a.id : null) === ("id" in b ? b.id : null);
  }

  /**
   * After a page change, focus goes to the new page's title (made focusable with tabindex -1),
   * not to the nav item that was used. Focus that survived the change on purpose stays: the
   * player bar, a dialog, or a row a return frame put back.
   */
  function focusPageTitle() {
    const active = document.activeElement as HTMLElement | null;
    const lost = !active || active === document.body || !active.isConnected;
    if (!lost && !active.closest(".app-sidebar")) return;
    // A page still loading may have no title yet: the main region takes focus instead.
    const target = mainEl?.querySelector<HTMLElement>("h1") ?? mainEl;
    if (!target) return;
    if (!target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
    target.focus({ preventScroll: true });
  }

  /** Resolves once the new page is in the DOM and focused, before callers move focus themselves. */
  function setRoute(next: Route): Promise<void> {
    const show = async () => {
      route = next;
      await tick();
      focusPageTitle();
    };
    // Only the scroll region cross-fades (a transition scoped to it, transitions.css): the glass
    // above it keeps refracting the live page and ambient light on every frame.
    const scope = mainEl as (HTMLElement & Pick<Document, "startViewTransition">) | undefined;
    if (typeof scope?.startViewTransition === "function" && !motionReduced) {
      const transition = scope.startViewTransition(show);
      return Promise.resolve(transition?.updateCallbackDone).then(
        () => undefined,
        () => undefined
      );
    }
    return show();
  }

  function navigate(next: Route): Promise<void> {
    if (sameRoute(next, route)) return Promise.resolve();
    lyricsReturn = null;
    frames.clear();
    history = [...history.slice(-(MAX_HISTORY - 1)), route];
    return setRoute(next);
  }

  function back(): Promise<void> {
    const prev = history.at(-1);
    if (!prev) return Promise.resolve();
    lyricsReturn = null;
    frames.clear();
    history = history.slice(0, -1);
    return setRoute(prev);
  }

  /** Account and page identity of a route; a search page includes its query. */
  function pageIdentity(r: Route): string {
    const id = "id" in r ? r.id : "";
    return `${accountKey}|${r.view}|${id}|${r.view === "search" ? (store.search.key ?? "") : ""}`;
  }

  /**
   * The dock button and Ctrl+L: open lyrics remembering the page, or close it again. Closing
   * goes back through the history (never pushes lyrics onto it) and puts the page back as it
   * was. With nothing to go back to, it shows Now Playing when a track is loaded, else Home.
   */
  function toggleLyrics(): Promise<void> {
    if (route.view === "lyrics") return closeLyrics();
    const left: LyricsReturn = {
      identity: pageIdentity(route),
      mainScrollTop: mainEl?.scrollTop ?? 0,
      list: frames.capture(),
    };
    const opened = navigate({ view: "lyrics" });
    lyricsReturn = left;
    return opened;
  }

  function closeLyrics(): Promise<void> {
    const left = lyricsReturn;
    const target = history.at(-1);
    if (!target) {
      lyricsReturn = null;
      return setRoute(currentTrack ? { view: "now-playing" } : { view: "home" });
    }
    const frame = left && left.identity === pageIdentity(target) ? left : null;
    const restoreMain = () => {
      if (frame && mainEl && sameRoute(route, target)) mainEl.scrollTop = frame.mainScrollTop;
    };
    const done = back();
    // back() drops any earlier offer; this one is for the page being returned to.
    if (frame?.list) frames.offer(frame.list, restoreMain);
    return done.then(restoreMain);
  }

  const openPlaylist = (id: string) => navigate({ view: "playlist", id });
  const openAlbum = (id: string) => navigate({ view: "album", id });
  const openArtist = (id: string) => navigate({ view: "artist", id });
  const openOfficialPlayer = () => void client.openPlayerWindow("auth_or_bridge_action");

  // The UI language follows core's settings; it stays as it is until the first snapshot.
  $effect(() => {
    if (settings) setLocale(settings.uiLanguage ?? "en");
  });

  // Apply theme, motion and transparency preferences to the document root.
  $effect(() => {
    if (typeof document === "undefined") return;
    const root = document.documentElement;
    // Always the resolved scheme ("system" is resolved here), so CSS needs one light palette.
    root.setAttribute("data-theme", colorScheme);
    root.setAttribute("data-reduce-motion", settings?.reduceMotion ? "true" : "false");
    root.setAttribute("data-reduce-transparency", settings?.reduceTransparency ? "true" : "false");
    root.lang = getLocale();
  });

  // Local position interpolation: only while playing, extrapolating and visible.
  $effect(() => {
    const playing = snapshot?.player.playback === "playing";
    const extrapolating = snapshot?.player.timeAnchor.extrapolating === true;
    const hidden = documentHidden;
    interpolatedPositionMs = client.getInterpolatedPositionMs({ hidden });
    if (!playing || !extrapolating || hidden) return;
    const timer = setInterval(() => {
      interpolatedPositionMs = client.getInterpolatedPositionMs({ hidden });
    }, 200);
    return () => clearInterval(timer);
  });

  function lyricsTrackMeta(track: Track) {
    return {
      videoId: track.videoId,
      title: track.title,
      artist: track.artists?.[0]?.name ?? t.common.unknownArtist,
      album: track.album?.title ?? null,
      durationMs: track.durationMs ?? null,
      versionTag: track.versionTag ?? null,
    };
  }

  /** Ignore older replies after a track change, cache clear, or offset adjustment. */
  async function requestLyrics(track: Track, change: { setUserOffsetMs?: number; resetUserOffset?: boolean } = {}) {
    const seq = ++lyricsSeq;
    lyricsLoading = true;
    let res: LyricsResult | null = null;
    try {
      res = await client.queryLyrics({
        track: lyricsTrackMeta(track),
        resetUserOffset: change.resetUserOffset ?? false,
        setUserOffsetMs: change.setUserOffsetMs ?? null,
      });
    } catch {
      res = null; // transport timeout: show "unavailable" rather than a stuck spinner
    }
    if (seq !== lyricsSeq) return;
    lyricsLoading = false;
    if (res && res.videoId !== currentTrack?.videoId) return;
    lyrics = res ?? lyrics;
  }

  const lyricsKey = $derived(
    currentTrack
      ? `${currentTrack.videoId}|${currentTrack.versionTag ?? ""}|${settings?.lrclibEnabled ?? true}|${clientState.cacheClearCount}`
      : ""
  );
  $effect(() => {
    const key = lyricsKey;
    // Now playing shows a lyrics preview, so it loads the same lyrics the Lyrics page does.
    const visible = (route.view === "lyrics" || route.view === "now-playing") && !documentHidden;
    if (key === requestedLyricsKey) return;
    requestedLyricsKey = visible ? key : null;
    untrack(() => {
      lyrics = null;
      lyricsSeq += 1;
      lyricsLoading = false;
      if (visible && key && currentTrack) void requestLyrics(currentTrack);
    });
  });

  function sendCommand(op: PlayerOp, args: PlayerCommandPayload["args"] = null): Promise<CommandReceipt | null> {
    return client.sendPlayerCommand(op, args);
  }

  /** Playlist context comes from a playlist page or an album's audio playlist; other lists play the single track. */
  function playTrack(track: Track, playlistId: string | null = null, index: number | null = null) {
    void sendCommand("playTarget", {
      target: {
        videoId: track.videoId,
        playlistId,
        startIndex: playlistId !== null ? index : null,
      },
    });
  }

  /** Play next / Add to end of queue sent from this window, by command id, until the player settles them. */
  const sentEnqueues = new Map<string, { readonly position: EnqueuePosition; readonly title: string }>();

  /**
   * The actions dialog closes at the click and the queue is not in view, so a confirmed enqueue
   * is reported as a toast (not on the Queue page, where the row itself appears). A failure is
   * reported by the command toasts below.
   */
  async function enqueue(track: Track, position: EnqueuePosition) {
    const receipt = await sendCommand("enqueue", {
      target: { videoId: track.videoId, playlistId: null, startIndex: null },
      enqueuePosition: position,
      queueRevision: snapshot?.queueSummary.queueRevision ?? null,
    });
    if (!receipt) return;
    sentEnqueues.set(receipt.commandId, { position, title: track.title });
    // A receipt overwritten by a later command before it settled is never seen; keep the newest few.
    if (sentEnqueues.size > 16) sentEnqueues.delete(sentEnqueues.keys().next().value!);
    settleEnqueue(receipt);
    // The confirmation may have arrived before the receipt did.
    settleEnqueue(snapshot?.pending.lastReceipt ?? null);
  }

  function settleEnqueue(receipt: CommandReceipt | null) {
    const sent = receipt?.op === "enqueue" ? sentEnqueues.get(receipt.commandId) : undefined;
    if (!receipt || !sent) return;
    if (receipt.status === "observed") {
      sentEnqueues.delete(receipt.commandId);
      if (route.view !== "queue") {
        toasts.show({ kind: "enqueued", tone: "success", text: t.toast.enqueued[sent.position], detail: sent.title });
      }
    } else if (receipt.status === "failed" || receipt.status === "indeterminate" || receipt.status === "cancelled") {
      sentEnqueues.delete(receipt.commandId);
    }
  }

  $effect(() => {
    const receipt = snapshot?.pending.lastReceipt ?? null;
    untrack(() => settleEnqueue(receipt));
  });

  function likedState(track: Track | null): boolean | null {
    if (!track) return null;
    const override = likeOverrides.get(track.videoId);
    if (override !== undefined) return override;
    if (invalidatedRatings.has(track.videoId)) return store.freshRating(track.videoId);
    return track.isLiked ?? null;
  }

  /** What to say about a write's outcome. `confirmed` is omitted when the control already shows it; `unknown` is the tooltip. */
  interface WriteMessages {
    readonly confirmed?: string;
    readonly failed: string;
    readonly unknown: string;
  }

  /**
   * One explicit account write; never retried automatically. While pending, the control shows it.
   * The outcome is a toast only when it is not visible where the action happened, or went wrong.
   */
  async function runMutation(req: Omit<MutationRequest, "confirmedByUser">, messages: WriteMessages) {
    const key = `${req.kind}:${req.videoId}`;
    if (pendingWrites.has(key)) return;
    const sentFor = accountKey;
    const token = Symbol(key);
    const before = ratingSequence;
    pendingWrites.set(key, token);
    const result = await client.sendMutation({ ...req, confirmedByUser: true });
    if (pendingWrites.get(key) !== token || sentFor !== accountKey) return;
    pendingWrites.delete(key);
    if (result.outcome === "confirmed") {
      if (req.kind === "setLike" && req.liked != null && (invalidatedRatings.get(req.videoId) ?? 0) <= before) {
        likeOverrides.set(req.videoId, req.liked);
      }
      if (messages.confirmed) toasts.show({ kind: "write-done", tone: "success", text: messages.confirmed });
    } else if (result.outcome === "rejected") {
      toasts.show({
        kind: "write-failed",
        tone: "danger",
        text: messages.failed,
        detail: result.error ? `${result.error.code}: ${result.error.safeMessage}` : null,
      });
    } else {
      toasts.show({
        kind: "write-unknown",
        tone: "warning",
        text: t.toast.writeUnknown,
        detail: messages.unknown,
        action: { label: t.toast.openPlayer, run: openOfficialPlayer },
      });
    }
  }

  /** The heart changes by itself; only a failure or an unclear outcome is reported. */
  function setLike(track: Track, liked: boolean) {
    void runMutation(
      { kind: "setLike", videoId: track.videoId, liked, playlistId: null },
      {
        failed: (liked ? t.toast.likeFailed : t.toast.unlikeFailed).replace("{title}", track.title),
        unknown: t.toast.writeUnknownDetail.replace("{title}", track.title),
      }
    );
  }

  /** The playlist is not in view, so success is reported too. There is no remove call, so no Undo. */
  function addToPlaylist(track: Track, playlist: PlaylistSummary) {
    void runMutation(
      { kind: "addToPlaylist", videoId: track.videoId, playlistId: playlist.playlistId, liked: null },
      {
        confirmed: t.toast.added.replace("{playlist}", playlist.title),
        failed: t.toast.addFailed.replace("{title}", track.title).replace("{playlist}", playlist.title),
        unknown: t.toast.writeUnknownDetail.replace("{title}", track.title),
      }
    );
  }

  /** The latest playback command that failed or whose outcome is unknown; keyed so it is reported once. */
  const commandIssue = $derived.by(() => {
    const receipt = snapshot?.pending.lastReceipt ?? null;
    if (receipt?.status === "indeterminate") {
      return { key: `ind:${receipt.commandId}`, unknown: true, error: receipt.error ?? null, op: receipt.op };
    }
    const error = clientState.pendingOverlay.lastError ?? (receipt?.status === "failed" ? (receipt.error ?? null) : null);
    if (!error) return null;
    // The request is named only when this receipt is the one that failed with this error; a
    // transport error has no receipt, and the last one may belong to an earlier command.
    const own =
      receipt?.status === "failed" &&
      receipt.error?.code === error.code &&
      receipt.error.safeMessage === error.safeMessage;
    return {
      key: `err:${receipt?.commandId ?? ""}:${error.code}:${error.safeMessage}`,
      unknown: false,
      error,
      op: own ? receipt.op : null,
    };
  });
  let reportedCommandKey: string | null = null;
  $effect(() => {
    const issue = commandIssue;
    // A new command clears the transport error, so the same failure again is reported again.
    if (!issue) reportedCommandKey = null;
    if (!issue || issue.key === reportedCommandKey) return;
    reportedCommandKey = issue.key;
    untrack(() => {
      if (issue.unknown) {
        toasts.show({
          kind: "command-unknown",
          tone: "warning",
          text: t.toast.commandUnknown,
          detail: [t.toast.notRetried, issue.error?.safeMessage].filter(Boolean).join("\n"),
          action: { label: t.toast.openPlayer, run: openOfficialPlayer },
        });
      } else {
        toasts.show({
          kind: "command-failed",
          tone: "danger",
          text: issue.op ? t.toast.commandFailed[issue.op] : t.toast.commandFailedGeneric,
          detail: issue.error ? `${issue.error.code}: ${issue.error.safeMessage}` : null,
          action: viewDiagnosticsAction(),
        });
      }
    });
  });

  function viewDiagnosticsAction() {
    return { label: t.toast.viewDiagnostics, run: () => (diagnosticsOpen = true), testId: "toast-view-diagnostics" };
  }

  /** Account and connection problems: one sticky toast with one action, until resolved. */
  const problem = $derived(
    currentProblem({ mode: clientState.mode, connectionError: clientState.connectionError, snapshot })
  );
  const problemToast = $derived<Toast | null>(problem ? describeProblem(problem) : null);

  /** The state of the page the route shows, for pages that load catalog data. */
  const routePageView = $derived.by<PageView | null>(() => {
    switch (route.view) {
      case "home":
        return homeView;
      case "search":
        return searchView;
      case "library":
        return libraryView;
      case "playlist":
        return playlistView;
      case "album":
        return albumView;
      case "artist":
        return artistView;
      case "liked":
        return likedView;
      case "recent":
        return recentView;
      default:
        return null;
    }
  });

  /** Reloads the page in view the way its own Retry button does. */
  function retryRoute() {
    if (route.view !== "search") loadRoute(route, true);
    else if (!searchBlocked) void store.runSearch(store.search.key ?? "");
  }

  /**
   * Content kept after a failed refresh: a toast with Retry, keyed to this page, so it leaves
   * when the page changes or a refresh works. The open playlist picker counts as a page showing
   * the library. Offline content gets none (the offline toast says it), and an account or
   * connection problem outranks it.
   */
  const refreshToast = $derived.by<Toast | null>(() => {
    const failed = (view: PageView | null) => (view?.cached && view.error ? view.error : null);
    const pageError = failed(routePageView);
    const pickerError = !pageError && actionsFor && pickerRequested ? failed(libraryView) : null;
    const error = pageError ?? pickerError;
    if (!error) return null;
    return {
      id: pageError ? `refresh-${pageIdentity(route)}` : "refresh-playlist-picker",
      kind: "refresh-failed",
      tone: "warning",
      text: t.states.refreshFailedNotice,
      detail: `${error.code}: ${error.safeMessage}`,
      action: {
        label: t.states.retry,
        run: pageError ? retryRoute : () => loadRoute({ view: "library" }, true),
        testId: "toast-retry",
      },
    };
  });

  $effect(() => {
    const error = route.view === "lyrics"
      ? lyrics?.error
      : actionsFor && pickerRequested
        ? store.library.moreError ?? libraryView.error
        : routePageView?.error;
    if (error) lastContentError = error;
  });

  /** The shell looks up only operations named by a delta it received outside the DJ page. */
  let agentScope: string | null = null;
  let agentReadGeneration = 0;
  let agentVisibilityGeneration = 0;
  let agentReading = false;
  /** True for operations observed while queue announcements were eligible. */
  const changedOperations = new Map<string, boolean>();
  const reportedOperations = new Set<string>();
  let agentHandoff = $state.raw<AgentOperationSummary | null>(null);
  let agentToasts = $state.raw<AgentOperationSummary[]>([]);

  function agentScopeOf(state: AppSnapshot | null): string | null {
    return state ? `${state.coreEpoch}:${catalogAccountKey(state.authState)}` : null;
  }

  function canReadAgentNews(): boolean {
    return !documentHidden && route.view !== "agent-activity";
  }

  function clearAgentNews() {
    agentReadGeneration += 1;
    agentReading = false;
    changedOperations.clear();
    agentHandoff = null;
    agentToasts = [];
    if (toasts.current?.kind === "agent-changed") toasts.dismiss();
  }

  function syncAgentScope(scope: string | null) {
    if (scope === agentScope) return;
    agentScope = scope;
    reportedOperations.clear();
    clearAgentNews();
  }

  $effect(() => {
    const scope = agentScopeOf(snapshot);
    const onDjPage = route.view === "agent-activity";
    const hidden = documentHidden;
    untrack(() => {
      syncAgentScope(scope);
      if (onDjPage) clearAgentNews();
      else if (hidden) {
        agentVisibilityGeneration += 1;
        for (const key of changedOperations.keys()) changedOperations.set(key, false);
      } else void readAgentNews();
    });
  });

  /** Give each settlement time in the single transient slot without displacing listener feedback. */
  $effect(() => {
    const operation = agentToasts[0];
    if (!operation || toasts.current || !canReadAgentNews()) return;
    untrack(() => {
      agentToasts = agentToasts.slice(1);
      const resource = operation.resource === "interruption" ? "interruption" : "queue";
      toasts.show({ kind: "agent-changed", tone: "agent", text: t.toast.agentChanged[resource], detail: operation.principal });
    });
  });

  function noteAgentOperation(principal: string, operationId: string) {
    const scope = agentScopeOf(client.getState().snapshot);
    syncAgentScope(scope);
    if (scope === null || route.view === "agent-activity") return;
    changedOperations.set(operationKey(principal, operationId), !documentHidden);
    if (!documentHidden) queueMicrotask(() => void readAgentNews());
  }

  async function readAgentNews() {
    const scope = agentScope;
    if (scope === null || scope !== agentScopeOf(client.getState().snapshot) ||
      agentReading || changedOperations.size === 0 || !canReadAgentNews()) return;
    const generation = agentReadGeneration;
    const visibilityGeneration = agentVisibilityGeneration;
    const batch = new Map(changedOperations);
    changedOperations.clear();
    agentReading = true;
    try {
      const { operations } = await client.agentActivity({ action: "read" });
      if (generation !== agentReadGeneration || scope !== agentScopeOf(client.getState().snapshot) || route.view === "agent-activity") return;
      // An acknowledged handoff can remain stuck until the listener sees it. Look it up after
      // unhide, but do not replay queue settlements from the time the window was hidden.
      if (documentHidden) {
        for (const key of batch.keys()) changedOperations.set(key, false);
        return;
      }
      const settled: AgentOperationSummary[] = [];
      for (const operation of operations) {
        const key = operationKey(operation.principal, operation.operationId);
        if (visibilityGeneration !== agentVisibilityGeneration || !batch.get(key) ||
          reportedOperations.has(key) || !settledAgentChange(operation)) continue;
        reportedOperations.add(key);
        settled.push(operation);
      }
      agentHandoff = operations.find(pendingAccountHandoff) ?? null;
      if (settled.length) agentToasts = [...agentToasts, ...settled.reverse()];
    } catch {
      // The DJ page reports a failed read; the shell stays quiet.
    } finally {
      if (generation === agentReadGeneration) {
        agentReading = false;
        void readAgentNews();
      }
    }
  }

  const handoffToast = $derived<Toast | null>(
    agentHandoff && route.view !== "agent-activity"
      ? {
          id: `agent-handoff-${operationKey(agentHandoff.principal, agentHandoff.operationId)}`,
          kind: "agent-handoff",
          tone: "agent",
          text: t.toast.agentSignIn,
          detail: agentHandoff.principal,
          action: { label: t.toast.openPlayer, run: openOfficialPlayer },
        }
      : null
  );

  function describeProblem(kind: ProblemKind): Toast {
    const openPlayer = { label: t.toast.openPlayer, run: openOfficialPlayer, testId: "btn-open-official-player" };
    const base = { id: `problem-${kind}`, kind, tone: "danger" as const };
    switch (kind) {
      case "unconnected":
        return {
          ...base,
          tone: "warning",
          text: t.toast.notConnected,
          detail: clientState.connectionError,
          // No bridge to reconnect to; the mock preview is in Settings > Advanced.
          action: viewDiagnosticsAction(),
        };
      case "connection-error":
        return {
          ...base,
          text: t.toast.connectionFailed,
          detail: clientState.connectionError,
          action: { label: t.states.reconnect, run: () => void client.retryConnect(), testId: "btn-reconnect-core" },
        };
      case "offline":
        // Nothing in the app can bring the network back; Diagnostics shows what the core sees.
        return { ...base, tone: "warning", text: t.toast.offline, action: viewDiagnosticsAction() };
      case "signed-out":
        return { ...base, tone: "warning", text: t.toast.signedOut, detail: auth?.reason, action: openPlayer };
      case "auth-expired":
        return { ...base, text: t.toast.signInExpired, detail: auth?.reason, action: openPlayer };
      case "auth-blocked":
        return { ...base, text: t.toast.signInRefused, detail: auth?.reason, action: openPlayer };
      case "bridge-disconnected":
        return { ...base, text: t.toast.playerDisconnected, detail: snapshot?.hostHealth.lastError, action: openPlayer };
      case "unsupported":
        return { ...base, tone: "warning", text: t.toast.controlsUnsupported, action: viewDiagnosticsAction() };
    }
  }

  const openTrackActions = (track: Track, startAt?: "pick") => {
    pickerRequested = false;
    trackActionsRequest = { key: ++trackActionsSequence, track, startAt };
    trackActionsOpen = true;
  };
  const closeTrackActions = () => {
    pickerRequested = false;
    trackActionsOpen = false;
  };
  /** Closes the actions dialog, then acts on the track it was opened for. */
  function runTrackAction(action: (track: Track) => void) {
    const track = actionsFor?.track;
    closeTrackActions();
    if (track) action(track);
  }

  function handleScenarioChange(scenario: FakeScenario) {
    activeScenario = scenario;
    client.setMockScenario(scenario);
    store.clear();
    loadRoute(route, true);
  }

  /** Mock only (Settings > Advanced): the playlist to fill with 10,000 synthetic songs once it has loaded. */
  let testPlaylistId = $state<string | null>(null);

  function openTestPlaylist() {
    const id =
      store.library.items[0]?.playlistId ?? store.home.detail?.pinnedPlaylists?.[0]?.playlistId ?? "mock-pl-favorites";
    void navigate({ view: "playlist", id }).then(() => (testPlaylistId = id));
  }

  $effect(() => {
    const id = testPlaylistId;
    if (id === null) return;
    const here = route.view === "playlist" && route.id === id && mockMode;
    if (here && playlistView.kind !== "ready") return;
    untrack(() => {
      testPlaylistId = null;
      if (!here) return;
      store.playlist.items = createSyntheticTracks(10_000, "mock");
      store.playlist.nextCursor = null;
    });
  });

  function handleGlobalKeydown(e: KeyboardEvent) {
    const action = resolveShortcut(e, imeComposing);
    if (!action) return;
    switch (action) {
      case "focusSearch":
        e.preventDefault();
        void navigate({ view: "search" })
          .then(() => tick())
          .then(() => document.getElementById("ytm-search-input")?.focus());
        break;
      case "toggleLyrics":
        e.preventDefault();
        void toggleLyrics();
        break;
      case "previous":
        e.preventDefault();
        if (prevGate.enabled) void sendCommand("previous");
        break;
      case "next":
        e.preventDefault();
        if (nextGate.enabled) void sendCommand("next");
        break;
      case "togglePlay":
        e.preventDefault();
        if (playGate.enabled) void sendCommand("toggle");
        break;
      case "escape":
        // A dialog closes first, even when focus has left it.
        if (actionsFor || diagnosticsOpen) {
          e.preventDefault();
          closeTrackActions();
          diagnosticsOpen = false;
        } else if (route.view === "lyrics" && lyricsReturn) {
          e.preventDefault();
          void closeLyrics();
        } else if (history.length > 0) {
          e.preventDefault();
          void back();
        }
        break;
    }
  }

  let unsubClient: (() => void) | null = null;

  onMount(() => {
    const onVisibility = () => {
      documentHidden = document.hidden;
      if (!document.hidden) client.resetInterpolationAnchor();
    };
    const onCompStart = () => {
      imeComposing = true;
    };
    const onCompEnd = () => {
      imeComposing = false;
    };
    const motionMq = typeof window.matchMedia === "function" ? window.matchMedia("(prefers-reduced-motion: reduce)") : null;
    const lightMq = typeof window.matchMedia === "function" ? window.matchMedia("(prefers-color-scheme: light)") : null;
    const onMotion = () => {
      prefersReducedMotion = motionMq?.matches ?? false;
    };
    const onScheme = () => {
      prefersLight = lightMq?.matches ?? false;
    };
    onMotion();
    onScheme();
    motionMq?.addEventListener("change", onMotion);
    lightMq?.addEventListener("change", onScheme);

    documentHidden = document.hidden;
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("compositionstart", onCompStart, true);
    window.addEventListener("compositionend", onCompEnd, true);
    window.addEventListener("keydown", handleGlobalKeydown);

    const dirtyRoutes = new Set<CatalogRoute>();
    let refreshQueued = false;
    let mounted = true;
    const unsubInvalidations = client.subscribeInvalidations(({ resource_kind: kind, resource_id: id }) => {
      if (kind === "rating" && id) {
        likeOverrides.delete(id);
        invalidatedRatings.set(id, ++ratingSequence);
      } else if (kind === "catalog" || kind === "resync") {
        for (const videoId of likeOverrides.keys()) invalidatedRatings.set(videoId, ++ratingSequence);
        likeOverrides.clear();
      } else if (kind === "lyricsOffset") {
        // Another client saved this track's offset: reread it so the next nudge starts from it.
        const track = untrack(() => currentTrack);
        if (track && id === track.videoId && requestedLyricsKey !== null) void requestLyrics(track);
      }
      for (const affected of store.invalidate(kind, id ?? null)) dirtyRoutes.add(affected);
      if (refreshQueued || dirtyRoutes.size === 0) return;
      refreshQueued = true;
      queueMicrotask(() => {
        refreshQueued = false;
        if (!mounted) return;
        const current = untrack(() => route);
        if (dirtyRoutes.has(current.view as CatalogRoute)) loadRoute(current);
        if (dirtyRoutes.has("library") && actionsFor && pickerRequested) loadRoute({ view: "library" });
        dirtyRoutes.clear();
      });
    });
    const unsubOperations = client.subscribeControlOperations(({ principal, operation_id }) => {
      noteAgentOperation(principal, operation_id);
    });
    unsubClient = client.subscribe((next) => { clientState = next; });
    void client.connect();

    return () => {
      mounted = false;
      unsubInvalidations();
      unsubOperations();
      clearAgentNews();
      motionMq?.removeEventListener("change", onMotion);
      lightMq?.removeEventListener("change", onScheme);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("compositionstart", onCompStart, true);
      window.removeEventListener("compositionend", onCompEnd, true);
      window.removeEventListener("keydown", handleGlobalKeydown);
    };
  });

  onDestroy(() => {
    sidebar.dispose();
    unsubClient?.();
    client.disconnect();
  });
</script>

<div
  class="app-shell"
  data-sidebar={sidebar.collapsed ? "collapsed" : "expanded"}
  data-sidebar-motion={sidebar.moving ? "" : undefined}
  ontransitionend={(event) => sidebar.onTransitionEnd(event)}
>
  <AmbientLayer
    store={ambient}
    playing={currentTrack ? { cover: currentTrack.cover, seed: currentTrack.videoId, label: currentTrack.title } : null}
    {mockMode}
    breathing={ambientBreathing}
    reducedMotion={motionReduced}
    scroller={mainEl}
  />

  <Sidebar
    activeView={navView}
    collapsed={sidebar.collapsed}
    textHidden={sidebar.textHidden}
    {mockMode}
    playback={snapshot?.player.playback ?? null}
    {connection}
    motion={decorativeMotion}
    onNavigate={(view) => navigate({ view })}
    reducedMotion={motionReduced}
    onToggleCollapse={() => sidebar.toggle(motionReduced)}
  />

  <main
    bind:this={mainEl}
    class="app-scroll view-panel-enter view-panel-active"
    aria-label={t.app.mainContentLabel}
    data-testid="main-scroll-region"
  >
    <!-- The content area (the `content` container the tiers query) and the page column in it. -->
    <div class="app-content">
      <div class="app-page">
        {#if route.view === "home"}
          <HomeView
            view={homeView}
            homeFeed={store.home.detail}
            {currentVideoId}
            like={rowLike}
            nowPlaying={rowPlayback}
            {mockMode}
            smoothScroll={!motionReduced}
            onPlayTrack={(t) => playTrack(t)}
            onTrackActions={openTrackActions}
            onOpenPlaylist={openPlaylist}
            onOpenRecent={() => navigate({ view: "recent" })}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
          />
        {:else if route.view === "search"}
          <SearchView
            query={store.search.key ?? ""}
            view={searchView}
            songs={store.search.items}
            albums={store.search.detail?.albums ?? []}
            artists={store.search.detail?.artists ?? []}
            playlists={store.search.detail?.playlists ?? []}
            hasMore={store.search.nextCursor !== null}
            loadingMore={store.search.loadingMore}
            moreError={store.search.moreError}
            {currentVideoId}
            like={rowLike}
            nowPlaying={rowPlayback}
            {mockMode}
            blocked={searchBlocked}
            {accountKey}
            recentTracks={store.home.detail?.recentTracks ?? []}
            libraryPlaylists={store.home.detail?.pinnedPlaylists ?? []}
            smoothScroll={!motionReduced}
            startDataMissing={store.home.status === "idle"}
            onRequestStartData={() => loadRoute({ view: "home" })}
            onSearch={(q) => {
              if (!searchBlocked) void store.runSearch(q);
            }}
            onLoadMore={() => void store.loadMoreSearch()}
            onPlayTrack={(t) => playTrack(t)}
            onTrackActions={openTrackActions}
            onOpenAlbum={openAlbum}
            onOpenArtist={openArtist}
            onOpenPlaylist={openPlaylist}
            onRetry={() => {
              if (!searchBlocked) void store.runSearch(store.search.key ?? "");
            }}
            onOpenOfficialPlayer={openOfficialPlayer}
          />
        {:else if route.view === "library"}
          <LibraryView
            view={libraryView}
            playlists={store.library.items}
            hasMore={store.library.nextCursor !== null}
            loadingMore={store.library.loadingMore}
            moreError={store.library.moreError}
            {mockMode}
            onOpenPlaylist={openPlaylist}
            onOpenLiked={() => navigate({ view: "liked" })}
            onOpenRecent={() => navigate({ view: "recent" })}
            onLoadMore={() => void store.loadMoreLibrary()}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
          />
        {:else if route.view === "playlist"}
          {@const playlistId = route.id}
          <TrackCollectionView
            name="playlist"
            title={store.playlist.detail?.title ?? t.library.playlistTitle}
            subtitle={store.playlist.detail?.author ?? store.playlist.detail?.description ?? null}
            cover={store.playlist.detail?.cover}
            coverSeed={playlistId}
            view={playlistView}
            tracks={store.playlist.items}
            hasMore={store.playlist.nextCursor !== null}
            total={(store.library.items.find((p) => p.playlistId === playlistId) ??
              store.home.detail?.pinnedPlaylists?.find((p) => p.playlistId === playlistId))?.trackCount ?? null}
            loadingMore={store.playlist.loadingMore}
            moreError={store.playlist.moreError}
            {currentVideoId}
            {mockMode}
            playAll
            like={rowLike}
            nowPlaying={rowPlayback}
            onBack={history.length > 0 ? back : undefined}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
            onLoadMore={() => void store.loadMorePlaylist()}
            onPlayTrack={(t, idx) => playTrack(t, playlistId, idx)}
            onTrackActions={(t) => openTrackActions(t)}
          />
        {:else if route.view === "album"}
          {@const albumId = route.id}
          <AlbumView
            {albumId}
            album={store.album.detail}
            view={albumView}
            tracks={store.album.items}
            {currentVideoId}
            {mockMode}
            like={rowLike}
            nowPlaying={rowPlayback}
            onBack={history.length > 0 ? back : undefined}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
            onOpenArtist={openArtist}
            onPlayTrack={(t, idx) => {
              const context =
                store.album.key === albumId ? albumPlaybackContext(store.album.detail, store.album.items, idx) : null;
              playTrack(t, context?.playlistId ?? null, context?.startIndex ?? null);
            }}
            onTrackActions={(t) => openTrackActions(t)}
          />
        {:else if route.view === "artist"}
          <ArtistView
            artistId={route.id}
            artist={store.artist.detail}
            view={artistView}
            tracks={store.artist.items}
            {currentVideoId}
            {mockMode}
            like={rowLike}
            nowPlaying={rowPlayback}
            onBack={history.length > 0 ? back : undefined}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
            onOpenAlbum={openAlbum}
            onPlayTrack={(t) => playTrack(t)}
            onTrackActions={(t) => openTrackActions(t)}
          />
        {:else if route.view === "liked" || route.view === "recent"}
          {@const isLiked = route.view === "liked"}
          {@const page = isLiked ? store.liked : store.recent}
          <TrackCollectionView
            name={route.view}
            title={isLiked ? t.liked.title : t.recent.title}
            icon={isLiked ? "heart" : "clock"}
            view={isLiked ? likedView : recentView}
            tracks={page.items}
            hasMore={page.nextCursor !== null}
            loadingMore={page.loadingMore}
            moreError={page.moreError}
            {currentVideoId}
            {mockMode}
            like={rowLike}
            nowPlaying={rowPlayback}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
            onLoadMore={() => {
              if (isLiked) void store.loadMoreLiked();
            }}
            onPlayTrack={(t) => playTrack(t)}
            onTrackActions={(t) => openTrackActions(t)}
          />
        {:else if route.view === "now-playing"}
          <!-- "Playing from": the context playlist's name, only when a loaded page or the library names it. -->
          {@const contextId = snapshot?.player.playbackContext?.playlistId ?? null}
          {@const playingFrom = !contextId
            ? null
            : store.playlist.detail?.playlistId === contextId
              ? store.playlist.detail.title
              : store.album.detail?.audioPlaylistId === contextId
                ? store.album.detail.title
                : (store.library.items.find((p) => p.playlistId === contextId)?.title ?? null)}
          <NowPlayingView
            {snapshot}
            {interpolatedPositionMs}
            liked={likedState(currentTrack)}
            {likeGate}
            likePending={currentTrack ? pendingWrites.has(`setLike:${currentTrack.videoId}`) : false}
            {canOpenAlbum}
            {canOpenArtist}
            accent={trackAccent}
            {motionReduced}
            {lyrics}
            {playingFrom}
            onToggleLike={() => {
              if (currentTrack) setLike(currentTrack, likedState(currentTrack) !== true);
            }}
            onOpenActions={() => {
              if (currentTrack) openTrackActions(currentTrack);
            }}
            {playlistGate}
            onAddToPlaylist={() => {
              if (currentTrack) openTrackActions(currentTrack, "pick");
            }}
            onOpenAlbum={openAlbum}
            onOpenArtist={openArtist}
            onOpenLyrics={() => navigate({ view: "lyrics" })}
          />
        {:else if route.view === "queue"}
          <QueueView
            queueSummary={snapshot?.queueSummary ?? null}
            capabilities={caps}
            {connection}
            player={snapshot?.player ?? null}
            lastReceipt={snapshot?.pending.lastReceipt ?? null}
            {mockMode}
            motion={decorativeMotion}
            like={rowLike}
            bind:playedOpen={queuePlayedOpen}
            onCommand={sendCommand}
            onSelectEntry={(entry) =>
              void sendCommand("selectQueueEntry", {
                queueEntryId: entry.entryId,
                queueRevision: snapshot?.queueSummary.queueRevision ?? null,
              })}
            onTrackActions={openTrackActions}
          />
        {:else if route.view === "lyrics"}
          <LyricsView
            track={currentTrack}
            {lyrics}
            loading={lyricsLoading}
            {interpolatedPositionMs}
            canSeek={seekGate.enabled}
            {mockMode}
            followActive={!documentHidden && snapshot?.player.playback === "playing"}
            smoothScroll={!motionReduced}
            accent={trackAccent}
            onSeekMs={(ms) =>
              sendCommand("seek", { positionMs: ms, trackInstanceId: snapshot?.player.trackInstanceId ?? null })}
            onStepOffset={(delta) => {
              if (currentTrack && lyrics)
                void requestLyrics(currentTrack, { setUserOffsetMs: stepUserOffsetMs(lyrics.userOffsetMs, delta) });
            }}
            onResetOffset={() => {
              if (currentTrack) void requestLyrics(currentTrack, { resetUserOffset: true });
            }}
          />
        {:else if route.view === "agent-activity"}
          <AgentActivity
            {client}
            scopeKey={snapshot ? `${snapshot.coreEpoch}:${accountKey}` : null}
            revision={clientState.activityRevision}
            visible={!documentHidden && hasSnapshot}
            onOpenOfficialPlayer={openOfficialPlayer}
          />
        {:else if route.view === "settings"}
          <SettingsView
            {snapshot}
            mockScenario={clientState.mode === "mock" && mockMode
              ? { active: activeScenario, onSelect: handleScenarioChange, onOpenTestPlaylist: openTestPlaylist }
              : null}
            onUseMockPreview={clientState.mode === "unconnected" ? () => void client.activateMockMode("normal") : null}
            momentClient={client}
            momentScopeKey={snapshot ? `${snapshot.coreEpoch}:${accountKey}` : null}
            momentRevision={clientState.momentRevision}
            onUpdateSettings={(patch) => client.updateSettings(patch)}
            onSaveFailed={(message) =>
              toasts.show({ kind: "settings-failed", tone: "danger", text: t.settings.saveFailed, detail: message })}
            onOpenDiagnostics={() => {
              diagnosticsOpen = true;
            }}
            onOpenOfficialPlayer={openOfficialPlayer}
            onClearCache={() => client.clearCache()}
            onLogout={() => void client.logout()}
          />
        {/if}
      </div>
    </div>
  </main>

  <PlayerBar
    {snapshot}
    pendingOverlay={clientState.pendingOverlay}
    {interpolatedPositionMs}
    onCommand={sendCommand}
    {seek}
    onOpenNowPlaying={() => navigate({ view: "now-playing" })}
    lyricsOpen={route.view === "lyrics"}
    onToggleLyrics={() => void toggleLyrics()}
    queueOpen={route.view === "queue"}
    onOpenQueue={() => void navigate({ view: "queue" })}
    liked={likedState(currentTrack)}
    {likeGate}
    likePending={currentTrack ? pendingWrites.has(`setLike:${currentTrack.videoId}`) : false}
    onToggleLike={() => {
      if (currentTrack) setLike(currentTrack, likedState(currentTrack) !== true);
    }}
  />

  <ToastHost store={toasts} sticky={problemToast ?? refreshToast ?? handoffToast} />

  <!-- The per-track menu: an L4 overlay grown from its trigger, above the content area only. -->
  {#if trackActionsRequest}
    <TrackActions
      active={trackActionsOpen}
      openKey={trackActionsRequest.key}
      track={trackActionsRequest.track}
      startAt={trackActionsRequest.startAt}
      onClose={closeTrackActions}
      liked={likedState(trackActionsRequest.track)}
      {mockMode}
      {enqueueGate}
      {likeGate}
      {playlistGate}
      {canOpenAlbum}
      {canOpenArtist}
      pending={pendingWrites.has(`setLike:${trackActionsRequest.track.videoId}`) ||
        pendingWrites.has(`addToPlaylist:${trackActionsRequest.track.videoId}`)}
      playlistsView={libraryView}
      playlists={store.library.items}
      playlistsHasMore={store.library.nextCursor !== null}
      playlistsLoadingMore={store.library.loadingMore}
      playlistsMoreError={store.library.moreError}
      onPlay={() => runTrackAction((t) => playTrack(t))}
      onEnqueue={(position) => runTrackAction((t) => enqueue(t, position))}
      onSetLike={(liked) => runTrackAction((t) => setLike(t, liked))}
      onRequestPlaylists={() => {
        pickerRequested = true;
        loadRoute({ view: "library" });
      }}
      onLoadMorePlaylists={() => void store.loadMoreLibrary()}
      onRetryPlaylists={() => loadRoute({ view: "library" }, true)}
      onAddToPlaylist={(playlist) => runTrackAction((t) => addToPlaylist(t, playlist))}
      onOpenAlbum={(id) => runTrackAction(() => openAlbum(id))}
      onOpenArtist={(id) => runTrackAction(() => openArtist(id))}
    />
  {/if}

  <AccessibleDialog
    open={diagnosticsOpen}
    title={t.dialog.diagnosticsTitle}
    onClose={() => {
      diagnosticsOpen = false;
    }}
  >
    {#if snapshot}
      {@const player = snapshot.player}
      {@const rows = [
        [t.dialog.coreEpoch, snapshot.coreEpoch],
        [t.dialog.bridgeEpoch, snapshot.player.bridgeEpoch ?? "-"],
        [t.dialog.accountEpoch, snapshot.accountEpoch ?? "-"],
        [
          t.dialog.revision,
          `#${snapshot.revision}${t.common.parenthetical.replace("{text}", `${t.dialog.resnapshotCount}${t.common.colon}${clientState.resnapshotCount}`)}`,
        ],
        [t.dialog.connection, t.dialog.connectionStates[snapshot.player.connection]],
        [t.dialog.authState, t.settings.authStates[snapshot.authState.state]],
        [t.dialog.network, t.dialog.networkStates[snapshot.networkState]],
        // What the pages no longer show: the player's state and the full facts of the audio stream.
        [t.dialog.playbackState, t.player.playbackStates[player.playback]],
        [
          t.dialog.codec,
          player.codec
            ? `${player.codec}${player.audioItag != null ? t.common.parenthetical.replace("{text}", String(player.audioItag)) : ""}`
            : t.common.unknown,
        ],
        [t.dialog.audioMode, player.audioMode || t.common.unknown],
        [t.dialog.audioQuality, player.audioQuality ? t.player.quality[player.audioQuality] : t.common.unknown],
        [
          t.dialog.audioBitrate,
          player.audioBitrateKbps ? t.player.quality.kbps.replace("{n}", String(player.audioBitrateKbps)) : t.common.unknown,
        ],
        [t.dialog.pageQualityLabel, player.qualityLabel || t.common.unknown],
        [t.dialog.drmProtected, boolLabel(player.drmProtected)],
        [t.dialog.waylandNative, boolLabel(snapshot.hostHealth.waylandNative)],
        [t.dialog.gpuAccelerated, boolLabel(snapshot.hostHealth.gpuAccelerated)],
        [t.dialog.sandboxEnabled, boolLabel(snapshot.hostHealth.sandboxEnabled)],
        [t.dialog.trayAvailable, boolLabel(snapshot.hostHealth.trayAvailable)],
        [t.dialog.mprisActive, boolLabel(snapshot.hostHealth.mprisActive)],
        [t.dialog.secretStoreBackend, snapshot.hostHealth.secretStoreBackend || t.common.unknown],
        [t.dialog.coreRestartCount, String(snapshot.hostHealth.coreRestartCount)],
        // Detail that toasts leave out: present only when there is something to report.
        ...(snapshot.hostHealth.lastError ? [[t.dialog.hostLastError, snapshot.hostHealth.lastError]] : []),
        ...(snapshot.authState.reason ? [[t.dialog.authReason, snapshot.authState.reason]] : []),
        ...(commandIssue?.error
          ? [[t.dialog.lastCommandError, `${commandIssue.error.code}${t.common.colon}${commandIssue.error.safeMessage}`]]
          : []),
        ...(lastContentError ? [[t.dialog.lastContentError, queryErrorDetail(lastContentError)]] : []),
      ]}
      <div class="flex flex-col gap-4 type-sm" data-testid="diagnostics-content">
        <dl class="grid grid-cols-1 sm:grid-cols-2 gap-2.5 m-0">
          {#each rows as [label, value] (label)}
            <div class="p-3 rounded-lg border" style="border-color: var(--border-subtle);">
              <dt class="type-xs" style="color: var(--text-muted);">{label}</dt>
              <dd class="font-mono type-xs mt-1 m-0 break-all">{value}</dd>
            </div>
          {/each}
        </dl>
        <div>
          <h3 class="type-sm font-semibold mb-2">{t.dialog.capabilitiesTitle}</h3>
          <div class="grid grid-cols-2 sm:grid-cols-3 gap-2 type-xs">
            {#each Object.entries(snapshot.capabilities) as [string, CapabilitySupport][] as [capKey, capSupport] (capKey)}
              <div
                class="flex items-center justify-between gap-2 px-2.5 py-1.5 rounded border"
                style="border-color: var(--border-subtle);"
                title={capKey}
              >
                <span class="truncate">{t.dialog.capabilityLabels[capKey] ?? capKey}</span>
                <span
                  class="shrink-0"
                  style="color: {capSupport === 'supported'
                    ? 'var(--status-success)'
                    : capSupport === 'unsupported'
                      ? 'var(--status-warning)'
                      : 'var(--text-muted)'};"
                >
                  {t.dialog.capabilitySupport[capSupport] ?? capSupport}
                </span>
              </div>
            {/each}
          </div>
        </div>
      </div>
    {:else}
      <div class="flex flex-col gap-3 type-sm" data-testid="diagnostics-content">
        <p class="m-0" style="color: var(--text-secondary);">{t.dialog.noSnapshot}</p>
        {#if clientState.connectionError}
          <div class="p-3 rounded-lg border" style="border-color: var(--border-subtle);">
            <div class="type-xs" style="color: var(--text-muted);">{t.dialog.connectionError}</div>
            <div class="font-mono type-xs mt-1 break-all">{clientState.connectionError}</div>
          </div>
        {/if}
      </div>
    {/if}
  </AccessibleDialog>
</div>
