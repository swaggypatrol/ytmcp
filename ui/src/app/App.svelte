<script lang="ts">
  import {
    createSyntheticTracks,
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
  import AccessibleDialog from "../lib/components/AccessibleDialog.svelte";
  import AgentActivity from "../lib/components/AgentActivity.svelte";
  import AlbumView from "../lib/components/AlbumView.svelte";
  import ArtistView from "../lib/components/ArtistView.svelte";
  import HomeView from "../lib/components/HomeView.svelte";
  import LibraryView from "../lib/components/LibraryView.svelte";
  import LyricsView from "../lib/components/LyricsView.svelte";
  import NowPlayingView from "../lib/components/NowPlayingView.svelte";
  import PlayerBar from "../lib/components/PlayerBar.svelte";
  import QueueView from "../lib/components/QueueView.svelte";
  import SearchView from "../lib/components/SearchView.svelte";
  import SettingsView from "../lib/components/SettingsView.svelte";
  import Sidebar from "../lib/components/Sidebar.svelte";
  import StatusBanners, { type MutationNotice } from "../lib/components/StatusBanners.svelte";
  import TrackActions from "../lib/components/TrackActions.svelte";
  import TrackCollectionView from "../lib/components/TrackCollectionView.svelte";
  import { getLocale, setLocale, t, type NavView } from "../lib/i18n/index.svelte.js";
  import { createUiClient, type UiClientState, type YtmUiClient } from "../lib/ipc/client.js";
  import { stepUserOffsetMs } from "../lib/lyrics.js";
  import { shouldRunDecorativeAnimation } from "../lib/motion/preferences.js";
  import { SeekControl } from "../lib/seek-control.svelte.js";
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

  let clientState = $state<UiClientState>(client.getState());
  let route = $state<Route>({ view: untrack(() => initialView) });
  let history = $state.raw<Route[]>([]);
  let sidebarCollapsed = $state(false);
  let activeScenario = $state<FakeScenario>("normal");
  let diagnosticsOpen = $state(false);
  let imeComposing = $state(false);
  let documentHidden = $state(false);
  let prefersReducedMotion = $state(false);
  let prefersLight = $state(false);
  let interpolatedPositionMs = $state(0);

  let lyrics = $state<LyricsResult | null>(null);
  let lyricsLoading = $state(false);
  let lyricsSeq = 0;
  let requestedLyricsKey: string | null = null;

  let actionsFor = $state<{ track: Track } | null>(null);
  let pickerRequested = false;
  let mutationNotice = $state<MutationNotice | null>(null);
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
  const trackAccent = $derived(accessibleTrackAccent(currentTrack?.cover?.accentOklch, colorScheme));

  const playGate = $derived(playPauseGate(caps?.playPause, connection, snapshot?.player.playback, currentTrack !== null));
  const seekGate = $derived(commandGate(caps?.seek, connection));
  const nextGate = $derived(commandGate(caps?.next, connection));
  const prevGate = $derived(commandGate(caps?.previous, connection));
  const enqueueGate = $derived(commandGate(caps?.queueEnqueue, connection));
  const likeGate = $derived(writeGate(caps?.likeMutation, auth));
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
      if (store.syncAccount(key)) {
        likeOverrides.clear();
        invalidatedRatings.clear();
        pendingWrites.clear();
        mutationNotice = null;
        closeTrackActions();
      }
      loadRoute(r);
    });
  });

  function sameRoute(a: Route, b: Route): boolean {
    return a.view === b.view && ("id" in a ? a.id : null) === ("id" in b ? b.id : null);
  }

  function setRoute(next: Route): Promise<void> {
    if (typeof document !== "undefined" && typeof document.startViewTransition === "function" && !motionReduced) {
      const transition = document.startViewTransition(async () => {
        route = next;
        await tick();
      });
      return Promise.resolve(transition?.updateCallbackDone).then(
        () => undefined,
        () => undefined
      );
    }
    route = next;
    return tick();
  }

  function navigate(next: Route): Promise<void> {
    if (sameRoute(next, route)) return Promise.resolve();
    history = [...history.slice(-(MAX_HISTORY - 1)), route];
    return setRoute(next);
  }

  function back(): Promise<void> {
    const prev = history.at(-1);
    if (!prev) return Promise.resolve();
    history = history.slice(0, -1);
    return setRoute(prev);
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
    const visible = route.view === "lyrics" && !documentHidden;
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

  function enqueue(track: Track, position: EnqueuePosition) {
    void sendCommand("enqueue", {
      target: { videoId: track.videoId, playlistId: null, startIndex: null },
      enqueuePosition: position,
      queueRevision: snapshot?.queueSummary.queueRevision ?? null,
    });
  }

  function likedState(track: Track | null): boolean | null {
    if (!track) return null;
    const override = likeOverrides.get(track.videoId);
    if (override !== undefined) return override;
    if (invalidatedRatings.has(track.videoId)) return store.freshRating(track.videoId);
    return track.isLiked ?? null;
  }

  /** One explicit account write; outcome is reported, never retried automatically. */
  async function runMutation(req: Omit<MutationRequest, "confirmedByUser">, label: string) {
    const key = `${req.kind}:${req.videoId}`;
    if (pendingWrites.has(key)) return;
    const sentFor = accountKey;
    const token = Symbol(key);
    const before = ratingSequence;
    pendingWrites.set(key, token);
    mutationNotice = { tone: "pending", text: t.mutation.pending.replace("{label}", label) };
    const result = await client.sendMutation({ ...req, confirmedByUser: true });
    if (pendingWrites.get(key) !== token || sentFor !== accountKey) return;
    pendingWrites.delete(key);
    if (result.outcome === "confirmed") {
      mutationNotice = { tone: "success", text: t.mutation.confirmed.replace("{label}", label) };
      if (req.kind === "setLike" && req.liked != null && (invalidatedRatings.get(req.videoId) ?? 0) <= before) {
        likeOverrides.set(req.videoId, req.liked);
      }
    } else if (result.outcome === "rejected") {
      mutationNotice = {
        tone: "error",
        text: t.mutation.rejected
          .replace("{label}", label)
          .replace("{message}", result.error?.safeMessage ?? result.error?.code ?? t.common.unknown),
      };
    } else {
      mutationNotice = { tone: "unknown", text: t.mutation.indeterminate.replace("{label}", label) };
    }
  }

  function setLike(track: Track, liked: boolean) {
    const label = (liked ? t.mutation.likeLabel : t.mutation.unlikeLabel).replace("{title}", track.title);
    void runMutation({ kind: "setLike", videoId: track.videoId, liked, playlistId: null }, label);
  }

  function addToPlaylist(track: Track, playlist: PlaylistSummary) {
    const label = t.mutation.addLabel.replace("{title}", track.title).replace("{playlist}", playlist.title);
    void runMutation({ kind: "addToPlaylist", videoId: track.videoId, playlistId: playlist.playlistId, liked: null }, label);
  }

  const openTrackActions = (track: Track) => {
    pickerRequested = false;
    actionsFor = { track };
  };
  const closeTrackActions = () => {
    pickerRequested = false;
    actionsFor = null;
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
      case "openLyrics":
        e.preventDefault();
        navigate({ view: "lyrics" });
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
        if (history.length > 0) {
          e.preventDefault();
          back();
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
    unsubClient = client.subscribe((next) => { clientState = next; });
    void client.connect();

    return () => {
      mounted = false;
      unsubInvalidations();
      motionMq?.removeEventListener("change", onMotion);
      lightMq?.removeEventListener("change", onScheme);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("compositionstart", onCompStart, true);
      window.removeEventListener("compositionend", onCompEnd, true);
      window.removeEventListener("keydown", handleGlobalKeydown);
    };
  });

  onDestroy(() => {
    unsubClient?.();
    client.disconnect();
  });
</script>

<div class="app-shell" data-sidebar={sidebarCollapsed ? "collapsed" : "expanded"}>
  <Sidebar
    activeView={navView}
    collapsed={sidebarCollapsed}
    {mockMode}
    isPlaying={snapshot?.player.playback === "playing"}
    onNavigate={(view) => navigate({ view })}
    onToggleCollapse={() => {
      sidebarCollapsed = !sidebarCollapsed;
    }}
  />

  <main
    class="app-scroll view-panel-enter view-panel-active"
    aria-label={t.app.mainContentLabel}
    data-testid="main-scroll-region"
  >
    <div class="app-page">
      <StatusBanners
        mode={clientState.mode}
        {snapshot}
        connectionError={clientState.connectionError}
        transportError={clientState.pendingOverlay.lastError}
        lastReceipt={snapshot?.pending.lastReceipt ?? null}
        {mutationNotice}
        onDismissMutation={() => {
          mutationNotice = null;
        }}
        {activeScenario}
        onSelectScenario={handleScenarioChange}
        onOpenOfficialPlayer={openOfficialPlayer}
        onActivateMock={() => {
          void client.activateMockMode("normal");
        }}
        onRetryConnect={() => {
          void client.retryConnect();
        }}
      />

      <div class="py-5">
        {#if route.view === "home"}
          <HomeView
            view={homeView}
            homeFeed={store.home.detail}
            {currentVideoId}
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
            {mockMode}
            blocked={searchBlocked}
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
            loadingMore={store.playlist.loadingMore}
            moreError={store.playlist.moreError}
            {currentVideoId}
            {mockMode}
            onBack={history.length > 0 ? back : undefined}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
            onLoadMore={() => void store.loadMorePlaylist()}
            onPlayTrack={(t, idx) => playTrack(t, playlistId, idx)}
            onTrackActions={(t) => openTrackActions(t)}
          >
            {#snippet header()}
              {#if mockMode && playlistView.kind === "ready" && store.playlist.items.length < 10_000}
                <button
                  type="button"
                  class="btn-ghost self-start"
                  onclick={() => {
                    store.playlist.items = createSyntheticTracks(10_000, "mock");
                    store.playlist.nextCursor = null;
                  }}
                  data-testid="btn-load-10k-fixture"
                >
                  {t.library.load10kFixture}
                </button>
              {/if}
            {/snippet}
          </TrackCollectionView>
        {:else if route.view === "album"}
          {@const albumId = route.id}
          <AlbumView
            {albumId}
            album={store.album.detail}
            view={albumView}
            tracks={store.album.items}
            {currentVideoId}
            {mockMode}
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
            view={isLiked ? likedView : recentView}
            tracks={page.items}
            hasMore={page.nextCursor !== null}
            loadingMore={page.loadingMore}
            moreError={page.moreError}
            {currentVideoId}
            {mockMode}
            onRetry={() => loadRoute(route, true)}
            onOpenOfficialPlayer={openOfficialPlayer}
            onLoadMore={() => {
              if (isLiked) void store.loadMoreLiked();
            }}
            onPlayTrack={(t) => playTrack(t)}
            onTrackActions={(t) => openTrackActions(t)}
          />
        {:else if route.view === "now-playing"}
          <NowPlayingView
            {snapshot}
            {interpolatedPositionMs}
            liked={likedState(currentTrack)}
            {likeGate}
            likePending={currentTrack ? pendingWrites.has(`setLike:${currentTrack.videoId}`) : false}
            canSeek={seekGate.enabled}
            {canOpenAlbum}
            {canOpenArtist}
            accent={trackAccent}
            {decorativeMotion}
            {seek}
            onSeekMs={(ms) =>
              sendCommand("seek", { positionMs: ms, trackInstanceId: snapshot?.player.trackInstanceId ?? null })}
            onToggleLike={() => {
              if (currentTrack) setLike(currentTrack, likedState(currentTrack) !== true);
            }}
            onOpenActions={() => {
              if (currentTrack) openTrackActions(currentTrack);
            }}
            onOpenAlbum={openAlbum}
            onOpenArtist={openArtist}
            onOpenLyrics={() => navigate({ view: "lyrics" })}
            onOpenQueue={() => navigate({ view: "queue" })}
          />
        {:else if route.view === "queue"}
          <QueueView
            queueSummary={snapshot?.queueSummary ?? null}
            capabilities={caps}
            {connection}
            player={snapshot?.player ?? null}
            lastReceipt={snapshot?.pending.lastReceipt ?? null}
            {mockMode}
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
            resnapshotCount={clientState.resnapshotCount}
            momentClient={client}
            momentScopeKey={snapshot ? `${snapshot.coreEpoch}:${accountKey}` : null}
            momentRevision={clientState.momentRevision}
            onUpdateSettings={(patch) => client.updateSettings(patch)}
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
    onOpenLyrics={() => navigate({ view: "lyrics" })}
    onOpenDiagnostics={() => {
      diagnosticsOpen = true;
    }}
  />

  <AccessibleDialog open={actionsFor !== null} title={t.track.actions} onClose={closeTrackActions}>
    {#if actionsFor}
      <TrackActions
        track={actionsFor.track}
        liked={likedState(actionsFor.track)}
        {mockMode}
        {enqueueGate}
        {likeGate}
        {playlistGate}
        {canOpenAlbum}
        {canOpenArtist}
        pending={pendingWrites.has(`setLike:${actionsFor.track.videoId}`) ||
          pendingWrites.has(`addToPlaylist:${actionsFor.track.videoId}`)}
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
  </AccessibleDialog>

  <AccessibleDialog
    open={diagnosticsOpen}
    title={t.dialog.diagnosticsTitle}
    onClose={() => {
      diagnosticsOpen = false;
    }}
  >
    {#if snapshot}
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
        [t.dialog.waylandNative, boolLabel(snapshot.hostHealth.waylandNative)],
        [t.dialog.gpuAccelerated, boolLabel(snapshot.hostHealth.gpuAccelerated)],
        [t.dialog.sandboxEnabled, boolLabel(snapshot.hostHealth.sandboxEnabled)],
        [t.dialog.trayAvailable, boolLabel(snapshot.hostHealth.trayAvailable)],
        [t.dialog.mprisActive, boolLabel(snapshot.hostHealth.mprisActive)],
        [t.dialog.secretStoreBackend, snapshot.hostHealth.secretStoreBackend || t.common.unknown],
        [t.dialog.coreRestartCount, String(snapshot.hostHealth.coreRestartCount)],
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
      <p class="type-sm" style="color: var(--text-secondary);">{t.states.loading}</p>
    {/if}
  </AccessibleDialog>
</div>
