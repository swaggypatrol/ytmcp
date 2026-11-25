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
    type MutationResult,
    type PlayerCommandPayload,
    type PlayerOp,
    type PlaylistSummary,
    type QueryError,
    type QueueEntry,
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
  import { HeldArtwork } from "../lib/ambient/held-artwork.svelte.js";
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
  import { RecentSearches } from "../lib/recent-searches.svelte.js";
  import SettingsView from "../lib/components/SettingsView.svelte";
  import Sidebar from "../lib/components/Sidebar.svelte";
  import { SidebarMotion } from "../lib/components/sidebar-motion.svelte.js";
  import ToastHost from "../lib/components/ToastHost.svelte";
  import TrackActions from "../lib/components/TrackActions.svelte";
  import TrackCollectionView from "../lib/components/TrackCollectionView.svelte";
  import { ToastStore, currentProblem, type ProblemKind, type Toast } from "../lib/feedback/toasts.svelte.js";
  import { ProblemLog } from "../lib/feedback/problems.svelte.js";
  import RecentProblems from "../lib/components/RecentProblems.svelte";
  import {
    operationKey,
    pendingAccountHandoff,
    settledAgentChange,
  } from "../lib/feedback/agent-news.js";
  import { getLocale, setLocale, t, type NavView } from "../lib/i18n/index.svelte.js";
  import { commandFailureMessage, queryErrorDetail } from "../lib/i18n/query-errors.js";
  import { createUiClient, type UiClientState, type YtmUiClient } from "../lib/ipc/client.js";
  import { effectiveLineTimeMs, stepUserOffsetMs } from "../lib/lyrics.js";
  import { shouldRunDecorativeAnimation } from "../lib/motion/preferences.js";
  import { nextPositionTickMs } from "../lib/position-clock.js";
  import { SeekControl } from "../lib/seek-control.svelte.js";
  import { ReturnFrames, setReturnFrames, type ListPosition } from "../lib/return-frame.js";
  import { resolveShortcut } from "../lib/shortcuts.js";
  import { accessibleTrackAccent } from "../lib/theme/accent.js";
  import { COMMAND_INTENT_TIMEOUT_MS } from "../lib/command-intent.svelte.js";
  import { isTerminalCommandStatus } from "../lib/seek-preview.js";

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
  /** Warnings and failures of this account, listed in Diagnostics. */
  const problems = new ProblemLog();
  /** The account the problem log belongs to, read fresh when a problem is logged. */
  function problemScopeNow(): string | null {
    const state = client.getState().snapshot;
    return state ? catalogAccountKey(state.authState) : null;
  }
  /** The one transient toast; account and connection problems are derived below. */
  const toasts = new ToastStore(problems, problemScopeNow);

  // Raw: the client replaces the state (and only the snapshot parts that changed) on every
  // notify and never mutates it, so unchanged parts keep their identity and the deriveds that
  // read them stop there instead of re-running for every position tick.
  let clientState = $state.raw<UiClientState>(client.getState());
  let route = $state<Route>({ view: untrack(() => initialView) });
  /** Pages left by forward moves, newest last; Back returns to the last one as it was left. */
  let history = $state.raw<LeftPage[]>([]);
  /** Collapse state of the sidebar and its transition (the shell's data-sidebar and data-sidebar-motion). */
  const sidebar = new SidebarMotion();
  let activeScenario = $state<FakeScenario>("normal");
  let diagnosticsOpen = $state(false);
  /** The problem entry Diagnostics lands on, when opened from a toast. */
  let problemFocus = $state.raw<{ id: string; seq: number } | null>(null);
  let lastContentError = $state.raw<QueryError | null>(null);
  let imeComposing = $state(false);
  let documentHidden = $state(false);
  let prefersReducedMotion = $state(false);
  let prefersLight = $state(false);
  let interpolatedPositionMs = $state(0);
  let mainEl = $state<HTMLElement | null>(null);

  /** A page left by a forward move, and how it looked: its scroll offset and its list's position. */
  interface LeftPage {
    readonly route: Route;
    /** `pageIdentity` when it was left; only the same account and query get the position back. */
    readonly identity: string;
    readonly mainScrollTop: number;
    readonly list: ListPosition | null;
  }
  /** The Queue page's Played section, kept open across leaving the page so its scroll position still fits. */
  let queuePlayedOpen = $state(false);

  let lyrics = $state<LyricsResult | null>(null);
  let lyricsLoading = $state(false);
  /** The last lyrics received had synced lines; Now Playing keeps that layout while the next ones load. */
  let lastLyricsSynced = $state(false);
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
  /** The heart a listener last asked for, per video, while its write (or the one after it) is on its way. */
  const likeRequests = new SvelteMap<string, boolean>();
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
  /** The Search page's recent searches, kept by the core for the signed-in account. */
  const recentSearches = new RecentSearches(client);
  const recentSearchEpoch = $derived(auth?.state === "authenticated" ? (auth.accountEpoch ?? null) : null);
  // Asked for as soon as an account is signed in, whatever page is open, so the list is
  // usually there before the Search page is.
  $effect(() => {
    const epoch = recentSearchEpoch;
    untrack(() => recentSearches.sync(epoch));
  });

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
  /** The playing cover for the bar, the lyrics header and the ambient light: the previous one stays while a new one is cached. */
  const playingArt = new HeldArtwork(
    () => (currentTrack ? { cover: currentTrack.cover, seed: currentTrack.videoId, label: currentTrack.title } : null),
    () => mockMode
  );

  const playGate = $derived(
    playPauseGate(caps?.playPause, connection, snapshot?.player.playback, currentTrack !== null, snapshot?.player.preparing === true)
  );
  const seekGate = $derived(commandGate(caps?.seek, connection));
  const nextGate = $derived(commandGate(caps?.next, connection));
  const prevGate = $derived(commandGate(caps?.previous, connection));
  const enqueueGate = $derived(commandGate(caps?.queueEnqueue, connection));
  /** Starting a queue entry, from the Queue page or Now Playing's Up next. */
  const queueSelectGate = $derived(commandGate(caps?.queueMirror, connection));
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
  // Every playerChanged brings a new `player`; the rows get a new object only when one of these values changes.
  const playbackState = $derived(snapshot?.player.playback ?? null);
  const rowPlayback = $derived({ playback: playbackState, connection, motion: decorativeMotion });
  const playlistGate = $derived(writeGate(caps?.playlistAddMutation, auth));
  const canOpenAlbum = $derived(caps?.albumDetail !== "unsupported");
  const canOpenArtist = $derived(caps?.artistDetail !== "unsupported");

  const boolLabel = (v: boolean | null | undefined) => (v == null ? t.common.unknown : v ? t.common.yes : t.common.no);

  const navView = $derived<NavView | null>(
    route.view === "playlist" ? "library" : route.view === "album" || route.view === "artist" ? null : route.view
  );

  type PageFields = Pick<CatalogPage<unknown, unknown>, "key" | "dataKey" | "status" | "error" | "hasData" | "stale">;
  const pick = (page: PageFields): PageFields => ({
    key: page.key,
    dataKey: page.dataKey,
    status: page.status,
    error: page.error,
    hasData: page.hasData,
    stale: page.stale,
  });

  /** `expectedKey` set: content for another id (previous playlist/album) counts as not loaded yet. */
  function pageView(
    view: Route["view"],
    page: PageFields,
    isEmpty: boolean,
    expectedKey?: string
  ): PageView {
    const otherKey = expectedKey !== undefined && page.key !== expectedKey;
    const otherData = expectedKey !== undefined && page.dataKey !== expectedKey;
    return resolvePageView({
      status: otherKey ? "loading" : page.status,
      error: otherKey ? null : page.error,
      hasData: otherKey || otherData ? false : page.hasData,
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
  /** The results shown are an earlier query's, on screen until the new query's reply. */
  const searchReplacing = $derived(store.search.dataKey !== null && store.search.dataKey !== store.search.key);
  const searchView = $derived.by(() => {
    const page = store.search;
    const isEmpty =
      page.items.length === 0 &&
      (page.detail?.albums?.length ?? 0) === 0 &&
      (page.detail?.artists?.length ?? 0) === 0 &&
      (page.detail?.playlists?.length ?? 0) === 0;
    // An earlier query's "no results" is never shown as the answer to the one still loading.
    const shown = searchReplacing && isEmpty ? { ...pick(page), hasData: false } : page;
    return pageView("search", shown, isEmpty);
  });
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

  /**
   * What this account's loaded pages already say about a collection, for its header before its
   * own page has loaded: the summary or reference the listener opened it from.
   */
  function knownPlaylist(id: string): PlaylistSummary | null {
    const match = (p: PlaylistSummary) => p.playlistId === id;
    return (
      store.library.items.find(match) ??
      store.home.detail?.pinnedPlaylists?.find(match) ??
      store.search.detail?.playlists?.find(match) ??
      null
    );
  }

  function knownAlbumTitle(id: string): string | null {
    const match = (a: { id?: string | null; browseId?: string | null }) => (a.id ?? a.browseId) === id;
    return store.search.detail?.albums?.find(match)?.title ?? store.artist.detail?.albums?.find(match)?.title ?? null;
  }

  function knownArtistName(id: string): string | null {
    const match = (a: { id?: string | null }) => a.id === id;
    return store.search.detail?.artists?.find(match)?.name ?? store.album.detail?.artists?.find(match)?.name ?? null;
  }

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
      if (store.syncAccount(key)) {
        likeOverrides.clear();
        likeRequests.clear();
        invalidatedRatings.clear();
        pendingWrites.clear();
        sentEnqueues.clear();
        collectionStart = null;
        clearAdditions();
        toasts.dismiss();
        lastContentError = null;
        closeTrackActions();
        // The pages stay in the history, but the old account's positions do not come back.
        history = history.map((left) => ({ ...left, identity: "", list: null }));
        frames.clear();
        stopScrollRestore?.();
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

  /** Longest a saved page offset waits for its page's content (Back to a page that loads again). */
  const SCROLL_RESTORE_WAIT_MS = 10_000;
  /** Keys that scroll the page; one pressed while a saved offset waits means the listener moved on. */
  const SCROLL_KEYS = new Set(["PageUp", "PageDown", "Home", "End", "ArrowUp", "ArrowDown", " "]);
  /** Ends the saved offset still waiting for its page's content, if any. */
  let stopScrollRestore: (() => void) | null = null;

  /**
   * Puts the page back at `top`. A page that has to load again is too short for that at first
   * (the offset is clamped), so the offset is set again as its content arrives, until it holds.
   * The listener scrolling first, another page change or a long wait ends it.
   */
  function restorePageScroll(top: number) {
    const el = mainEl;
    if (!el) return;
    const apply = () => {
      if (el.scrollTop !== top) el.scrollTop = top;
      return Math.abs(el.scrollTop - top) <= 1;
    };
    if (apply()) return;
    const controller = new AbortController();
    const observer = new MutationObserver(() => {
      if (apply()) stop();
    });
    const timer = setTimeout(() => stop(), SCROLL_RESTORE_WAIT_MS);
    const stop = () => {
      observer.disconnect();
      controller.abort();
      clearTimeout(timer);
      if (stopScrollRestore === stop) stopScrollRestore = null;
    };
    observer.observe(el, { childList: true, subtree: true });
    for (const type of ["wheel", "touchmove", "pointerdown"]) el.addEventListener(type, stop, { passive: true, signal: controller.signal });
    window.addEventListener("keydown", (event) => SCROLL_KEYS.has(event.key) && stop(), { capture: true, signal: controller.signal });
    stopScrollRestore = stop;
  }

  /**
   * Resolves once the new page is in the DOM and focused, before callers move focus themselves.
   * The page starts at its top, or at `saved` (Back): set inside the transition's update, so the
   * old page's snapshot is already taken and the new page is never seen at the old offset.
   */
  function setRoute(next: Route, saved: LeftPage | null = null): Promise<void> {
    const show = async () => {
      stopScrollRestore?.();
      // To the top before the new page mounts, so whatever it positions on mount stays; a saved
      // offset only once its content is there to scroll (a list's own frame puts it back once
      // its rows are there; any other page waits for its content here).
      if (!saved && mainEl && mainEl.scrollTop !== 0) mainEl.scrollTop = 0;
      // A re-read of the page left stops; it is read again when the listener comes back.
      if (!sameRoute(route, next)) store.leave(route.view);
      route = next;
      await tick();
      if (saved?.list) {
        if (mainEl && mainEl.scrollTop !== saved.mainScrollTop) mainEl.scrollTop = saved.mainScrollTop;
      } else if (saved) {
        restorePageScroll(saved.mainScrollTop);
      }
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

  /** A forward move: the page left goes on the history with how it looked, and the new one starts at its top. */
  function navigate(next: Route): Promise<void> {
    if (sameRoute(next, route)) return Promise.resolve();
    const left: LeftPage = {
      route,
      identity: pageIdentity(route),
      mainScrollTop: mainEl?.scrollTop ?? 0,
      list: frames.capture(),
    };
    frames.clear();
    history = [...history.slice(-(MAX_HISTORY - 1)), left];
    return setRoute(next);
  }

  /**
   * Back to the previous page, where it was scrolled (and its list's anchored row and focus,
   * once its rows are there; see return-frame.ts). A page of another account or another
   * search query is not put back.
   */
  function back(): Promise<void> {
    const prev = history.at(-1);
    if (!prev) return Promise.resolve();
    frames.clear();
    history = history.slice(0, -1);
    const saved = prev.identity === pageIdentity(prev.route) ? prev : null;
    if (saved?.list) {
      frames.offer(saved.list, () => {
        if (mainEl && sameRoute(route, saved.route)) mainEl.scrollTop = saved.mainScrollTop;
      });
    }
    return setRoute(prev.route, saved);
  }

  /** Account and page identity of a route; a search page includes its query. */
  function pageIdentity(r: Route): string {
    const id = "id" in r ? r.id : "";
    return `${accountKey}|${r.view}|${id}|${r.view === "search" ? (store.search.key ?? "") : ""}`;
  }

  /**
   * The dock button and Ctrl+L: open lyrics, or close it again. Closing goes back through the
   * history (never pushes lyrics onto it), so the page comes back as it was left. With nothing
   * to go back to, it shows Now Playing when a track is loaded, else Home.
   */
  function toggleLyrics(): Promise<void> {
    if (route.view === "lyrics") return closeLyrics();
    return navigate({ view: "lyrics" });
  }

  function closeLyrics(): Promise<void> {
    if (history.length > 0) return back();
    return setRoute(currentTrack ? { view: "now-playing" } : { view: "home" });
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

  /** Display times of the synced lines on screen (Lyrics or Now Playing), where a highlight must switch. */
  const lyricLineTimes = $derived(
    (route.view === "lyrics" || route.view === "now-playing") && lyrics?.kind === "synced"
      ? (lyrics.syncedLines ?? []).map((line) => effectiveLineTimeMs(line, lyrics!))
      : []
  );

  // Local position interpolation: only while playing, extrapolating and visible. One timer at a
  // time wakes on the next whole second of the shown time or the next lyric line, whichever is
  // first, and at least every 200 ms for the seek fill (nextPositionTickMs), so the seconds
  // advance evenly, highlights switch on time and the fill moves smoothly, without a busy loop.
  $effect(() => {
    const playing = snapshot?.player.playback === "playing";
    const extrapolating = snapshot?.player.timeAnchor.extrapolating === true;
    const rate = snapshot?.player.timeAnchor.playbackRate ?? 1;
    const lines = lyricLineTimes;
    const hidden = documentHidden;
    const read = () => {
      const positionMs = client.getInterpolatedPositionMs({ hidden });
      interpolatedPositionMs = positionMs;
      return positionMs;
    };
    const now = read();
    if (!playing || !extrapolating || hidden) return;
    let timer: ReturnType<typeof setTimeout>;
    const wakeAfter = (positionMs: number) => {
      timer = setTimeout(() => wakeAfter(read()), nextPositionTickMs(positionMs, rate, lines));
    };
    wakeAfter(now);
    return () => clearTimeout(timer);
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
    if (res) lastLyricsSynced = res.kind === "synced" && (res.syncedLines?.length ?? 0) > 0;
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
      // The old lines go at once (they never stand in for the new track); Now Playing keeps
      // only its layout until the answer (holdLyricsLayout).
      lyrics = null;
      lyricsSeq += 1;
      lyricsLoading = false;
      if (visible && key && currentTrack) void requestLyrics(currentTrack);
    });
  });

  function sendCommand(op: PlayerOp, args: PlayerCommandPayload["args"] = null): Promise<CommandReceipt | null> {
    return client.sendPlayerCommand(op, args);
  }

  let playerBar = $state<ReturnType<typeof PlayerBar>>();

  /** The shown position: the seek preview while one holds, so lyrics, the bar and the time move together. */
  const shownPositionMs = $derived(seek.preview?.positionMs ?? interpolatedPositionMs);

  /** A seek from outside the seek bar (a lyric line), through its preview: shown at once, reconciled like a drag. */
  function seekTo(positionMs: number) {
    const durationMs = snapshot?.player.timeAnchor.durationMs ?? currentTrack?.durationMs ?? null;
    void seek.commit(positionMs, durationMs, snapshot?.player.trackInstanceId, (ms, trackInstanceId) =>
      sendCommand("seek", { positionMs: ms, trackInstanceId })
    );
  }

  /** Starts a queue occurrence at the queue revision it was shown with; the one way both pages start an entry. */
  function selectQueueEntry(entry: QueueEntry): Promise<CommandReceipt | null> {
    return sendCommand("selectQueueEntry", {
      queueEntryId: entry.entryId,
      queueRevision: snapshot?.queueSummary.queueRevision ?? null,
    });
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

  /**
   * Play or Shuffle for the whole Liked songs collection, offered only while the page's own
   * playback context is fresh, was read for the signed-in account, and the player is ready.
   * The target is the first row with its native index; the app never names the playlist itself.
   */
  const likedCollection = $derived.by(() => {
    const page = store.liked;
    const context = page.detail;
    const first = page.items[0];
    const entry = context?.playback.entries[0];
    if (!context || !first || !entry || page.status !== "done" || page.stale || page.error) return null;
    if (auth?.state !== "authenticated" || !auth.accountScope || auth.accountScope !== context.accountScope) return null;
    if (connection !== "ready") return null;
    return {
      target: { videoId: first.videoId, playlistId: context.playback.playlistId, startIndex: entry.startIndex },
      canShuffle: caps?.shuffle === "supported",
    };
  });

  /** The collection start sent from this window, until its playCollection settles; one at a time. */
  let collectionStart = $state.raw<{ readonly mode: "play" | "shuffle"; readonly videoId: string; readonly commandId: string | null } | null>(null);

  /** Sends one playCollection; Shuffle (or an in-order Play) is set only once the page has started it. */
  async function startCollection(mode: "play" | "shuffle") {
    const collection = likedCollection;
    if (!collection || collectionStart || (mode === "shuffle" && !collection.canShuffle)) return;
    const start = { mode, videoId: collection.target.videoId, commandId: null };
    collectionStart = start;
    const receipt = await sendCommand("playCollection", { target: { ...collection.target } });
    if (collectionStart !== start) return;
    if (!receipt) {
      collectionStart = null;
      return;
    }
    collectionStart = { ...start, commandId: receipt.commandId };
    settleCollectionStart(receipt);
    // The outcome may have arrived before the reply did.
    settleCollectionStart(snapshot?.pending.lastReceipt ?? null);
  }

  /**
   * Ends the collection start on its own receipt. Only an observed start, still on the track it
   * started, is followed by setShuffle, and only when the page's shuffle differs from the mode.
   */
  function settleCollectionStart(receipt: CommandReceipt | null) {
    const start = collectionStart;
    if (!start?.commandId || receipt?.commandId !== start.commandId || !isTerminalCommandStatus(receipt.status)) return;
    collectionStart = null;
    if (receipt.status !== "observed" || currentVideoId !== start.videoId || !commandGate(caps?.shuffle, connection).enabled) return;
    const shuffle = snapshot?.player.shuffle ?? null;
    if (start.mode === "shuffle" && shuffle !== true) void sendCommand("setShuffle", { shuffle: true });
    else if (start.mode === "play" && shuffle === true) void sendCommand("setShuffle", { shuffle: false });
  }

  // Settles on the receipt; when this window has nothing pending any more, the receipt was
  // missed (a later one replaced it), so the start ends without a follow-up.
  $effect(() => {
    const receipt = snapshot?.pending.lastReceipt ?? null;
    const ownIdle = clientState.pendingOverlay.pendingCount === 0;
    untrack(() => {
      settleCollectionStart(receipt);
      if (collectionStart?.commandId && ownIdle) collectionStart = null;
    });
  });
  $effect(() => {
    if (!collectionStart) return;
    const timer = setTimeout(() => (collectionStart = null), COMMAND_INTENT_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });
  // Leaving the page, or another track starting, ends it: no follow-up fires later elsewhere.
  $effect(() => {
    const onLiked = route.view === "liked";
    const videoId = currentVideoId;
    untrack(() => {
      if (collectionStart && (!onLiked || (videoId !== null && videoId !== collectionStart.videoId))) collectionStart = null;
    });
  });

  function likedState(track: Track | null): boolean | null {
    if (!track) return null;
    const requested = likeRequests.get(track.videoId);
    if (requested !== undefined) return requested;
    const override = likeOverrides.get(track.videoId);
    if (override !== undefined) return override;
    if (invalidatedRatings.has(track.videoId)) return store.freshRating(track.videoId);
    return track.isLiked ?? null;
  }

  /**
   * What to say about a write's outcome. `confirmed` is omitted when the control already shows it,
   * or reports the confirmed result itself (an addition offering Undo); `unknown` is the tooltip.
   */
  interface WriteMessages {
    readonly confirmed?: string | ((result: MutationResult) => void);
    readonly failed: string;
    readonly unknown: string;
    /** The track (or playlist) the write was about, for the problem log. */
    readonly subject: string;
  }

  /**
   * One explicit account write; never retried automatically. While pending, the control shows it.
   * The outcome is a toast only when it is not visible where the action happened, or went wrong.
   * Resolves to the result, or null when the write was not sent or its answer no longer applies.
   */
  async function runMutation(
    req: Omit<MutationRequest, "confirmedByUser">,
    messages: WriteMessages
  ): Promise<MutationResult | null> {
    const key = `${req.kind}:${req.videoId}`;
    if (pendingWrites.has(key)) return null;
    const sentFor = accountKey;
    const token = Symbol(key);
    const before = ratingSequence;
    pendingWrites.set(key, token);
    const result = await client.sendMutation({ ...req, confirmedByUser: true });
    if (pendingWrites.get(key) !== token || sentFor !== accountKey) return null;
    pendingWrites.delete(key);
    if (result.outcome === "confirmed") {
      if (req.kind === "setLike" && req.liked != null && (invalidatedRatings.get(req.videoId) ?? 0) <= before) {
        likeOverrides.set(req.videoId, req.liked);
      }
      if (typeof messages.confirmed === "function") messages.confirmed(result);
      else if (messages.confirmed) toasts.show({ kind: "write-done", tone: "success", text: messages.confirmed });
    } else if (result.outcome === "rejected") {
      toasts.show({
        kind: "write-failed",
        tone: "danger",
        text: messages.failed,
        detail: queryErrorLine(result.error),
        action: viewDiagnosticsAction(),
        problem: { subject: messages.subject },
      });
    } else {
      showWriteUnknown(messages, result.error);
    }
    return result;
  }

  /** A write whose outcome is unknown: check the official page; it is logged and never retried. */
  function showWriteUnknown(messages: Pick<WriteMessages, "unknown" | "subject">, error: QueryError | null | undefined) {
    toasts.show({
      kind: "write-unknown",
      tone: "warning",
      text: t.toast.writeUnknown,
      detail: messages.unknown,
      action: { label: t.toast.openPlayer, run: openOfficialPlayer },
      problem: {
        subject: messages.subject,
        advice: t.dialog.checkWriteAdvice,
        technical: queryErrorLine(error),
        unknownOutcome: true,
      },
    });
  }

  /**
   * The heart turns at the click and shows as pending until the write settles; only a failure or
   * an unclear outcome is reported, and either one puts the heart back. A click while a write is
   * on its way changes the request: once that write is confirmed and the listener now wants the
   * other value, that is written next, so the heart ends where the last click left it.
   */
  function setLike(track: Track, liked: boolean) {
    likeRequests.set(track.videoId, liked);
    if (!pendingWrites.has(`setLike:${track.videoId}`)) void writeLike(track);
  }

  /** Writes the requested heart; a newer request waits for this one and goes next. */
  async function writeLike(track: Track) {
    const liked = likeRequests.get(track.videoId);
    if (liked === undefined) return;
    const result = await runMutation(
      { kind: "setLike", videoId: track.videoId, liked, playlistId: null },
      {
        failed: (liked ? t.toast.likeFailed : t.toast.unlikeFailed).replace("{title}", track.title),
        unknown: t.toast.writeUnknownDetail.replace("{title}", track.title),
        subject: track.title,
      }
    );
    if (result === null) return;
    const wanted = likeRequests.get(track.videoId);
    // Only after a confirmation: a refused or unclear write is never followed by another one.
    if (result.outcome === "confirmed" && wanted !== undefined && wanted !== liked) {
      void writeLike(track);
      return;
    }
    likeRequests.delete(track.videoId);
  }

  /**
   * The playlist is not in view, so success is reported too. It is sent at the pick: there is no
   * confirmation step, because a confirmed addition whose entry the provider named can be taken
   * back from its toast's Undo. One without a token is reported the same way, without Undo.
   */
  function addToPlaylist(track: Track, playlist: PlaylistSummary) {
    const text = t.toast.added.replace("{playlist}", playlist.title);
    const sentFor = accountKey;
    void runMutation(
      { kind: "addToPlaylist", videoId: track.videoId, playlistId: playlist.playlistId, liked: null },
      {
        confirmed: (result) => {
          if (!result.undoToken) {
            toasts.show({ kind: "write-done", tone: "success", text });
            return;
          }
          const addition: Addition = {
            token: result.undoToken,
            videoId: track.videoId,
            title: track.title,
            playlist,
            accountKey: sentFor,
            expiresAt: Date.now() + UNDO_TOKEN_LIFETIME_MS,
          };
          offerUndo(addition, { kind: "write-done", tone: "success", text });
        },
        failed: t.toast.addFailed.replace("{title}", track.title).replace("{playlist}", playlist.title),
        unknown: t.toast.writeUnknownDetail.replace("{title}", track.title),
        subject: `${track.title} → ${playlist.title}`,
      }
    );
  }

  /** How long this window keeps an addition's undo token; the core keeps none longer. */
  const UNDO_TOKEN_LIFETIME_MS = 5 * 60 * 1000;

  /** A confirmed addition of this account that its Undo can still take back. */
  interface Addition {
    readonly token: string;
    readonly videoId: string;
    readonly title: string;
    readonly playlist: PlaylistSummary;
    readonly accountKey: string;
    readonly expiresAt: number;
  }

  /** Undo tokens still usable, with the toast offering each; dropped when used, expired or on an account change. */
  const additions = new Map<string, { readonly addition: Addition; readonly timer: ReturnType<typeof setTimeout>; readonly toastId: string | null }>();
  /** Undos on their way, so a second press never sends a second one. */
  const undoing = new Set<string>();

  /** Shows `toast` with Undo for `addition`, which is kept until its lifetime ends (its toast then leaves too). */
  function offerUndo(addition: Addition, toast: Pick<Toast, "kind" | "tone" | "text">) {
    const remaining = addition.expiresAt - Date.now();
    toasts.show({ ...toast, action: { label: t.toast.undo, run: () => void undoAddition(addition.token), testId: "toast-undo" } });
    const toastId = toasts.current?.id ?? null;
    dropAddition(addition.token);
    const timer = setTimeout(() => {
      const kept = additions.get(addition.token);
      additions.delete(addition.token);
      if (kept?.toastId) toasts.dismiss(kept.toastId);
    }, remaining);
    additions.set(addition.token, { addition, timer, toastId });
  }

  function dropAddition(token: string) {
    const kept = additions.get(token);
    if (kept) clearTimeout(kept.timer);
    additions.delete(token);
  }

  function clearAdditions() {
    for (const { timer } of additions.values()) clearTimeout(timer);
    additions.clear();
    undoing.clear();
  }

  /**
   * Takes back one addition: the press is the confirmation, it is sent once and never retried.
   * While it is on its way the toast stays with its Undo marked pending. A refusal that sent
   * nothing and may work later offers Undo again; any other outcome ends the addition's Undo.
   */
  async function undoAddition(token: string) {
    if (undoing.has(token)) return;
    const kept = additions.get(token);
    if (!kept) {
      toasts.show({ kind: "undo-expired", tone: "neutral", text: t.toast.undoExpired });
      return;
    }
    const { addition } = kept;
    dropAddition(token);
    undoing.add(token);
    const { title, playlist } = addition;
    const subject = `${title} → ${playlist.title}`;
    toasts.show({
      kind: "write-done",
      tone: "success",
      text: t.toast.added.replace("{playlist}", playlist.title),
      action: { label: t.toast.undo, run: () => {}, pending: true, testId: "toast-undo" },
    });
    const sentFor = accountKey;
    const result = await client.sendMutation({
      kind: "undoAddToPlaylist",
      videoId: addition.videoId,
      playlistId: playlist.playlistId,
      undoToken: token,
      liked: null,
      confirmedByUser: true,
    });
    if (!undoing.delete(token) || sentFor !== accountKey) return;
    if (result.outcome === "confirmed") {
      toasts.show({ kind: "undo-done", tone: "success", text: t.toast.undone.replace("{playlist}", playlist.title) });
    } else if (result.outcome === "indeterminate") {
      showWriteUnknown({ unknown: t.toast.writeUnknownDetail.replace("{title}", title), subject }, result.error);
    } else if (result.alreadyRemoved) {
      toasts.show({ kind: "undo-already-removed", tone: "neutral", text: t.toast.undoAlreadyRemoved });
    } else if (result.error?.retriable && Date.now() < addition.expiresAt) {
      // Nothing was sent and the core kept the token: Undo stays (until its lifetime ends).
      offerUndo(addition, { kind: "undo-not-now", tone: "warning", text: t.toast.undoNotNow });
    } else if (result.error?.code === "STALE_CONTEXT" || result.error?.retriable) {
      toasts.show({ kind: "undo-expired", tone: "neutral", text: t.toast.undoExpired });
    } else {
      toasts.show({
        kind: "write-failed",
        tone: "danger",
        text: t.toast.undoFailed.replace("{title}", title).replace("{playlist}", playlist.title),
        detail: queryErrorLine(result.error),
        action: viewDiagnosticsAction(),
        problem: { subject },
      });
    }
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
          problem: { advice: t.dialog.checkCommandAdvice, technical: queryErrorLine(issue.error) },
        });
      } else {
        toasts.show({
          kind: "command-failed",
          tone: "danger",
          text: commandFailureMessage(issue.op, issue.error?.code),
          detail: issue.error ? `${issue.error.code}: ${issue.error.safeMessage}` : null,
          action: viewDiagnosticsAction(),
        });
      }
    });
  });

  /** A toast's way to Diagnostics: it opens on that toast's entry in Recent problems. */
  function viewDiagnosticsAction() {
    return {
      label: t.toast.viewDiagnostics,
      run: (toast: Toast) => openDiagnostics(problems.entryForToast(toast.id)?.id ?? null),
      testId: "toast-view-diagnostics",
    };
  }

  let problemFocusSeq = 0;
  function openDiagnostics(problemId: string | null = null) {
    diagnosticsOpen = true;
    problemFocus = problemId ? { id: problemId, seq: ++problemFocusSeq } : null;
  }

  /** "CODE: message" for the problem log and tooltips; null without an error. */
  function queryErrorLine(error: QueryError | null | undefined): string | null {
    return error ? `${error.code}: ${error.safeMessage}` : null;
  }

  /** Account and connection problems: one sticky toast with one action, until resolved. */
  const problem = $derived(
    currentProblem({ mode: clientState.mode, connectionError: clientState.connectionError, snapshot })
  );
  const problemToast = $derived<Toast | null>(problem ? describeProblem(problem) : null);

  /** The problem log belongs to one account; a core restart keeps it (see `ProblemLog`). */
  const problemScope = $derived(snapshot ? accountKey : null);

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

  // A new scope starts the log empty; then the account and connection problem and a failed
  // refresh, each logged once per appearance (one can hide the other on screen).
  $effect(() => {
    const scope = problemScope;
    const standing = [problemToast, refreshToast];
    untrack(() => {
      toasts.syncScope(scope);
      toasts.trackStanding(standing);
    });
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
        // The bar's own press: it means what the play button shows, and shows it at once.
        if (playGate.enabled) playerBar?.pressPlay();
        break;
      case "escape":
        // A dialog closes first, even when focus has left it.
        if (actionsFor || diagnosticsOpen) {
          e.preventDefault();
          closeTrackActions();
          diagnosticsOpen = false;
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
      toasts.setHidden(document.hidden);
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
    toasts.setHidden(document.hidden);
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("compositionstart", onCompStart, true);
    window.addEventListener("compositionend", onCompEnd, true);
    window.addEventListener("keydown", handleGlobalKeydown);

    const dirtyRoutes = new Set<CatalogRoute>();
    // Set when the library went stale for a reason other than a write to one named playlist.
    let libraryRereadInPicker = false;
    let refreshQueued = false;
    let mounted = true;
    const unsubInvalidations = client.subscribeInvalidations(({ resource_kind: kind, resource_id: id }) => {
      if (kind === "rating" && id) {
        likeOverrides.delete(id);
        invalidatedRatings.set(id, ++ratingSequence);
      } else if (kind === "catalog" || kind === "resync") {
        for (const videoId of likeOverrides.keys()) invalidatedRatings.set(videoId, ++ratingSequence);
        likeOverrides.clear();
      } else if (kind === "accountState") {
        recentSearches.invalidate();
      } else if (kind === "lyricsOffset") {
        // Another client saved this track's offset: reread it so the next nudge starts from it.
        const track = untrack(() => currentTrack);
        if (track && id === track.videoId && requestedLyricsKey !== null) void requestLyrics(track);
      }
      for (const affected of store.invalidate(kind, id ?? null)) {
        dirtyRoutes.add(affected);
        if (affected === "library" && !(kind === "libraryPlaylists" && id)) libraryRereadInPicker = true;
      }
      if (refreshQueued || dirtyRoutes.size === 0) return;
      refreshQueued = true;
      queueMicrotask(() => {
        refreshQueued = false;
        if (!mounted) return;
        const current = untrack(() => route);
        if (dirtyRoutes.has(current.view as CatalogRoute)) loadRoute(current);
        // An open playlist picker is read again, except when the only change is a write to one
        // named playlist (an add made from this picker): re-reading then would reorder the list
        // under the pointer while the listener picks the next one. The library stays stale and is
        // read again the next time the picker or the Library opens.
        if (dirtyRoutes.has("library") && libraryRereadInPicker && actionsFor && pickerRequested) loadRoute({ view: "library" });
        libraryRereadInPicker = false;
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
    stopScrollRestore?.();
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
    playing={playingArt.shown}
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
            replacing={searchReplacing}
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
            recentQueries={recentSearches.items}
            onRememberQuery={(q) => recentSearches.record(q)}
            onClearRecent={() => recentSearches.clear()}
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
          {@const detail = store.playlist.dataKey === playlistId ? store.playlist.detail : null}
          {@const summary = knownPlaylist(playlistId)}
          <TrackCollectionView
            name="playlist"
            title={detail?.title ?? summary?.title ?? t.library.playlistTitle}
            subtitle={detail?.author ?? detail?.description ?? null}
            cover={detail?.cover ?? summary?.cover}
            coverSeed={playlistId}
            view={playlistView}
            tracks={store.playlist.items}
            hasMore={store.playlist.nextCursor !== null}
            total={summary?.trackCount ?? null}
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
            album={store.album.dataKey === albumId ? store.album.detail : null}
            knownTitle={knownAlbumTitle(albumId)}
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
                store.album.dataKey === albumId ? albumPlaybackContext(store.album.detail, store.album.items, idx) : null;
              playTrack(t, context?.playlistId ?? null, context?.startIndex ?? null);
            }}
            onTrackActions={(t) => openTrackActions(t)}
          />
        {:else if route.view === "artist"}
          <ArtistView
            artistId={route.id}
            artist={store.artist.dataKey === route.id ? store.artist.detail : null}
            knownName={knownArtistName(route.id)}
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
            collection={isLiked && likedCollection
              ? {
                  pending: collectionStart?.mode ?? null,
                  canShuffle: likedCollection.canShuffle,
                  onPlay: () => void startCollection("play"),
                  onShuffle: () => void startCollection("shuffle"),
                }
              : null}
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
            interpolatedPositionMs={shownPositionMs}
            artwork={playingArt.shown}
            liked={likedState(currentTrack)}
            {likeGate}
            likePending={currentTrack ? pendingWrites.has(`setLike:${currentTrack.videoId}`) : false}
            {canOpenAlbum}
            {canOpenArtist}
            accent={trackAccent}
            {motionReduced}
            {lyrics}
            holdLyricsLayout={lyricsLoading && lastLyricsSynced}
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
            onOpenQueue={() => void navigate({ view: "queue" })}
            selectGate={queueSelectGate}
            onSelectEntry={selectQueueEntry}
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
            onSelectEntry={(entry) => void selectQueueEntry(entry)}
            onTrackActions={openTrackActions}
          />
        {:else if route.view === "lyrics"}
          <LyricsView
            track={currentTrack}
            {lyrics}
            loading={lyricsLoading}
            interpolatedPositionMs={shownPositionMs}
            canSeek={seekGate.enabled}
            {mockMode}
            smoothScroll={!motionReduced}
            accent={trackAccent}
            artwork={playingArt.shown}
            onSeekMs={seekTo}
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
              toasts.show({
                kind: "settings-failed",
                tone: "danger",
                text: t.settings.saveFailed,
                detail: message,
                action: viewDiagnosticsAction(),
              })}
            onOpenDiagnostics={() => openDiagnostics()}
            problems={problems.entries.length
              ? { count: problems.entries.length, failed: problems.entries.some((entry) => entry.tone === "danger") }
              : null}
            onOpenOfficialPlayer={openOfficialPlayer}
            onClearCache={() => client.clearCache()}
            onLogout={() => void client.logout()}
          />
        {/if}
      </div>
    </div>
  </main>

  <PlayerBar
    bind:this={playerBar}
    {snapshot}
    pendingOverlay={clientState.pendingOverlay}
    {interpolatedPositionMs}
    onCommand={sendCommand}
    {seek}
    artwork={playingArt.shown}
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
      problemFocus = null;
    }}
  >
    <div class="flex flex-col gap-4">
      <RecentProblems
        entries={problems.entries}
        focus={problemFocus}
        onMarkChecked={(id) => problems.markChecked(id)}
        onOpenOfficialPlayer={openOfficialPlayer}
      />
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
    </div>
  </AccessibleDialog>
</div>
