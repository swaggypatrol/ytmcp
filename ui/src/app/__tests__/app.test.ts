import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import {
  FakeCoreClient,
  PROTOCOL_VERSION,
  createSyntheticTracks,
  type AppSnapshot,
  type CatalogQueryRequest,
  type CatalogResponsePayload,
  type CommandReceipt,
  type Envelope,
  type LyricsResult,
  type MutationRequest,
  type MutationResult,
  type PlayerCommandPayload,
  type StateDelta,
} from "@ytmcp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import { tick } from "svelte";
import App from "../App.svelte";
import PlayerBar from "../../lib/components/PlayerBar.svelte";
import VirtualizedTrackList from "../../lib/components/VirtualizedTrackList.svelte";
import {
  CoreRequestError,
  IpcTimeoutError,
  createUiClient,
  wrapPreloadEnvelopeBridge,
  type RawPreloadUiBridge,
  type YtmcpUiBridge,
} from "../../lib/ipc/client.js";
import { LONG_CJK_MIXED_SAMPLE_TRACK } from "../../fixtures/synthetic.js";
import { en, type NavView } from "../../lib/i18n/en.js";
import { setLocale } from "../../lib/i18n/index.svelte.js";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  setLocale("en");
});

const DISABLED_LYRICS: LyricsResult = {
  videoId: "mock-vid-00001",
  kind: "disabled",
  source: "disabled",
  language: null,
  matchBasis: null,
  fetchedAtMs: 0,
  fileOffsetMs: 0,
  userOffsetMs: 0,
  syncedLines: [],
  plainText: null,
  error: null,
};

interface BridgeOptions {
  snapshot?: (base: AppSnapshot) => AppSnapshot;
  queryCatalog?: (req: CatalogQueryRequest) => CatalogResponsePayload | Promise<CatalogResponsePayload>;
  sendMutation?: YtmcpUiBridge["sendMutation"];
  queryLyrics?: YtmcpUiBridge["queryLyrics"];
}

/** A non-mock preload bridge backed by the synthetic fake core, with per-test overrides. */
function createPreloadBridge(options: BridgeOptions = {}) {
  const fake = new FakeCoreClient("normal");
  const base: AppSnapshot = { ...fake.subscribeSnapshot(() => {}).snapshot, mockMode: false };
  const snapshot = options.snapshot ? options.snapshot(base) : base;
  let listener: ((delta: StateDelta) => void) | null = null;
  const catalogCalls: CatalogQueryRequest[] = [];
  const bridge = {
    subscribeSnapshot: (l: (delta: StateDelta) => void) => {
      listener = l;
      return { snapshot: structuredClone(snapshot), unsubscribe: () => (listener = null) };
    },
    sendPlayerCommand: vi.fn<(cmd: PlayerCommandPayload) => CommandReceipt | Promise<CommandReceipt>>((cmd) =>
      fake.sendPlayerCommand(cmd)
    ),
    queryCatalog: vi.fn((req: CatalogQueryRequest) => {
      catalogCalls.push(req);
      return options.queryCatalog ? options.queryCatalog(req) : fake.queryCatalog(req);
    }),
    queryLyrics: options.queryLyrics ?? (() => DISABLED_LYRICS),
    updateSettings: vi.fn(),
    momentSettings: vi.fn((req) => fake.momentSettings(req)),
    openPlayerWindow: vi.fn(),
    clearCache: vi.fn(),
    sendMutation: options.sendMutation,
    logout: vi.fn(),
  } satisfies YtmcpUiBridge;
  return { bridge, snapshot, catalogCalls, emit: (d: StateDelta) => listener?.(d) };
}

function renderApp(bridge: YtmcpUiBridge, initialView: NavView = "home") {
  return render(App, { props: { client: createUiClient({ preloadBridge: bridge }), initialView } });
}

const writeReady = (s: AppSnapshot): AppSnapshot => ({
  ...s,
  capabilities: {
    ...s.capabilities,
    likeMutation: "supported",
    playlistAddMutation: "supported",
    queueEnqueue: "supported",
  },
  authState: { ...s.authState, state: "authenticated", playerCatalogMatched: true },
});

function paged<T>(items: T[], nextCursor: string | null = null) {
  return {
    items,
    nextCursor,
    freshness: "fresh" as const,
    fetchedAtMs: 0,
    accountScope: "scope-test",
    providerVersion: "test",
  };
}

describe("YtmUiClient protocol & timing semantics", () => {
  it("buffers listener-first deltas and re-snapshots on revision gaps", async () => {
    const fake = new FakeCoreClient("normal");
    const client = createUiClient({ fakeClient: fake });
    await client.connect();
    expect(client.getState().mode).toBe("mock");
    expect(client.getState().snapshot?.revision).toBe(1);

    client.simulateRevisionGap();
    await client.sendPlayerCommand("play");
    await waitFor(() => expect(client.getState().resnapshotCount).toBeGreaterThanOrEqual(1));
    client.disconnect();
  });

  it("does not mutate observed player state when setting optimistic pending overlay", async () => {
    const holder: { resolve?: (val: CommandReceipt) => void } = {};
    const fake = new FakeCoreClient("normal");
    const { bridge } = createPreloadBridge();
    bridge.sendPlayerCommand.mockImplementation(
      () => new Promise<CommandReceipt>((resolve) => (holder.resolve = resolve)) as never
    );
    const client = createUiClient({ preloadBridge: bridge });
    await client.connect();

    const cmdPromise = client.sendPlayerCommand("play");
    expect(client.getState().pendingOverlay.pendingCount).toBe(1);
    expect(client.getState().pendingOverlay.desiredPlayback).toBe("playing");
    expect(client.getState().snapshot?.player.playback).toBe("paused");

    holder.resolve?.(fake.sendPlayerCommand({ commandId: "c-1", op: "play", args: null }));
    await cmdPromise;
    client.disconnect();
  });

  it("keeps each local pending command until its own terminal receipt", async () => {
    const { bridge, emit, snapshot } = createPreloadBridge();
    bridge.sendPlayerCommand.mockImplementation((cmd): CommandReceipt => ({
      commandId: cmd.commandId,
      op: cmd.op,
      status: "accepted",
      acceptedAtMs: 0,
      observedAtMs: null,
      error: null,
      deduplicated: false,
    }));
    const client = createUiClient({ preloadBridge: bridge });
    await client.connect();
    const play = (await client.sendPlayerCommand("play"))!;
    const seek = (await client.sendPlayerCommand("seek", { positionMs: 21_000 }))!;
    expect(client.getState().pendingOverlay).toMatchObject({ pendingCount: 2, desiredPlayback: "playing", desiredPositionMs: 21_000 });
    emit({
      deltaType: "commandUpdated",
      revision: snapshot.revision + 1,
      receipt: { ...play, commandId: "external-next", op: "next", status: "failed" },
    });
    expect(client.getState().pendingOverlay.pendingCount).toBe(2);
    emit({ deltaType: "commandUpdated", revision: snapshot.revision + 2, receipt: { ...play, status: "observed" } });
    expect(client.getState().pendingOverlay).toMatchObject({ pendingCount: 1, activeOp: "seek", desiredPositionMs: 21_000 });
    emit({ deltaType: "commandUpdated", revision: snapshot.revision + 3, receipt: { ...seek, status: "observed" } });
    expect(client.getState().pendingOverlay).toMatchObject({ pendingCount: 0, desiredPositionMs: null });
    client.disconnect();
  });

  it("freezes interpolation while hidden but keeps the clock running under reduced motion", async () => {
    const { bridge, emit } = createPreloadBridge({
      snapshot: (s) => ({ ...s, settings: { ...s.settings, reduceMotion: true } }),
    });
    const client = createUiClient({ preloadBridge: bridge });
    await client.connect();
    const snap = client.getState().snapshot!;
    const now = vi.spyOn(performance, "now");
    now.mockReturnValue(1_000);
    emit({
      deltaType: "playerChanged",
      revision: snap.revision + 1,
      pending: snap.pending,
      player: {
        ...snap.player,
        playback: "playing",
        timeAnchor: { positionMs: 10_000, durationMs: 180_000, playbackRate: 1, anchorCoreMonotonicMs: 0, extrapolating: true },
      },
    });
    now.mockReturnValue(3_000);
    expect(client.getInterpolatedPositionMs({ hidden: true })).toBe(10_000);
    expect(client.getInterpolatedPositionMs({ hidden: false })).toBe(12_000);
    client.disconnect();
  });
});

describe("App page states and mock isolation", () => {
  it("renders mock scenarios through the shared page states and hides mock UI in preload mode", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }) } });
    await waitFor(() => expect(screen.getByTestId("banner-mock-mode")).toBeTruthy());
    const select = screen.getByTestId("mock-scenario-select") as HTMLSelectElement;

    await fireEvent.change(select, { target: { value: "loading" } });
    await waitFor(() => expect(screen.getByTestId("home-state-loading")).toBeTruthy());

    await fireEvent.change(select, { target: { value: "empty" } });
    await waitFor(() => expect(screen.getByTestId("home-state-empty")).toBeTruthy());

    await fireEvent.change(select, { target: { value: "offlineCached" } });
    await waitFor(() => {
      expect(screen.getByTestId("banner-offline")).toBeTruthy();
      expect(screen.getByTestId("home-state-cached")).toBeTruthy();
    });

    await fireEvent.change(select, { target: { value: "authExpired" } });
    await waitFor(() => {
      expect(screen.getByTestId("banner-auth-expired")).toBeTruthy();
      expect(screen.getByTestId("home-state-auth-expired")).toBeTruthy();
    });

    await fireEvent.change(select, { target: { value: "unsupported" } });
    await waitFor(() => {
      expect(screen.getByTestId("banner-unsupported")).toBeTruthy();
      expect((screen.getByTestId("btn-play-pause") as HTMLButtonElement).disabled).toBe(true);
    });

    await fireEvent.change(select, { target: { value: "bridgeCrashed" } });
    await waitFor(() => {
      expect(screen.getByTestId("banner-bridge-disconnected")).toBeTruthy();
      expect((screen.getByTestId("btn-play-pause") as HTMLButtonElement).disabled).toBe(true);
    });
    expect(fetchSpy).not.toHaveBeenCalled();

    cleanup();
    renderApp(createPreloadBridge().bridge);
    await waitFor(() => expect(screen.getByTestId("view-home")).toBeTruthy());
    expect(screen.queryByTestId("status-banners")).toBeNull();
    expect(screen.queryByTestId("banner-mock-mode")).toBeNull();
    expect(screen.queryByTestId("mock-scenario-select")).toBeNull();
  });

  it("shows unsupported, auth-expired, error and cached states per page without fake data", async () => {
    const unsupported = createPreloadBridge({
      snapshot: (s) => ({ ...s, capabilities: { ...s.capabilities, likedSongs: "unsupported" } }),
    });
    renderApp(unsupported.bridge, "liked");
    await waitFor(() => expect(screen.getByTestId("liked-state-unsupported")).toBeTruthy());
    expect(unsupported.catalogCalls.some((r) => r.kind === "likedSongs")).toBe(false);
    cleanup();

    render(App, { props: { client: createUiClient({ mockScenario: "authExpired" }), initialView: "library" } });
    await waitFor(() => expect(screen.getByTestId("library-state-auth-expired")).toBeTruthy());
    cleanup();

    const failing = createPreloadBridge({
      queryCatalog: (req) =>
        req.kind === "libraryPlaylists"
          ? { resultType: "error", code: "RATE_LIMITED", retriable: true, retryAfterMs: 1000, safeMessage: "slow down", requestId: null }
          : new FakeCoreClient("normal").queryCatalog(req),
    });
    renderApp(failing.bridge, "library");
    const errorBox = await screen.findByTestId("library-state-error");
    expect(errorBox.textContent).toContain("RATE_LIMITED");
    await fireEvent.click(within(errorBox).getByRole("button"));
    await waitFor(() => expect(failing.catalogCalls.filter((r) => r.kind === "libraryPlaylists")).toHaveLength(2));
    cleanup();

    render(App, { props: { client: createUiClient({ mockScenario: "offlineCached" }), initialView: "library" } });
    await waitFor(() => {
      expect(screen.getByTestId("library-state-cached")).toBeTruthy();
      expect(screen.getByTestId("playlist-card-mock-pl-favorites")).toBeTruthy();
    });
  });
});

describe("Page loading follows capability and sign-in", () => {
  it("loads a page once its capability turns supported, without a route or account change", async () => {
    const { bridge, snapshot, catalogCalls, emit } = createPreloadBridge({
      snapshot: (s) => ({ ...s, capabilities: { ...s.capabilities, likedSongs: "unsupported" } }),
    });
    renderApp(bridge, "liked");
    await waitFor(() => expect(screen.getByTestId("liked-state-unsupported")).toBeTruthy());
    expect(catalogCalls.some((r) => r.kind === "likedSongs")).toBe(false);

    emit({
      deltaType: "capabilitiesChanged",
      revision: snapshot.revision + 1,
      capabilities: { ...snapshot.capabilities, likedSongs: "supported" },
    });
    await waitFor(() => expect(catalogCalls.filter((r) => r.kind === "likedSongs")).toHaveLength(1));
    await waitFor(() => expect(screen.queryByTestId("liked-state-loading")).toBeNull());
    expect(screen.queryByTestId("liked-state-unsupported")).toBeNull();
  });

  it("asks for sign-in on private and core-gated pages while signed out, and sends no request", async () => {
    // The real core reports catalog reads unsupported whenever nobody is signed in.
    const signedOut = (s: AppSnapshot): AppSnapshot => ({
      ...s,
      authState: { ...s.authState, state: "expired", accountEpoch: null, playerCatalogMatched: false },
      capabilities: { ...s.capabilities, search: "unsupported", likedSongs: "unsupported", libraryPlaylists: "unsupported" },
    });
    for (const view of ["liked", "library", "recent"] as const) {
      const { bridge, catalogCalls } = createPreloadBridge({ snapshot: signedOut });
      renderApp(bridge, view);
      await waitFor(() => expect(screen.getByTestId(`${view}-state-auth-expired`)).toBeTruthy());
      expect(catalogCalls).toHaveLength(0);
      cleanup();
    }

    const { bridge, catalogCalls } = createPreloadBridge({ snapshot: signedOut });
    renderApp(bridge, "search");
    await waitFor(() => expect(screen.getByTestId("search-state-auth-expired")).toBeTruthy());
    const input = screen.getByTestId("search-input") as HTMLInputElement;
    expect(input.disabled).toBe(true);
    await fireEvent.submit(input.closest("form")!);
    expect(catalogCalls.some((r) => r.kind === "searchSongs")).toBe(false);
  });

  it("shows unsupported search without a query and never sends it", async () => {
    const { bridge, catalogCalls } = createPreloadBridge({
      snapshot: (s) => ({ ...s, capabilities: { ...s.capabilities, search: "unsupported" } }),
    });
    renderApp(bridge, "search");
    await waitFor(() => expect(screen.getByTestId("search-state-unsupported")).toBeTruthy());
    const input = screen.getByTestId("search-input") as HTMLInputElement;
    expect(input.disabled).toBe(true);
    await fireEvent.input(input, { target: { value: "abc" } });
    await fireEvent.submit(input.closest("form")!);
    await new Promise((r) => setTimeout(r, 300));
    expect(catalogCalls.some((r) => r.kind === "searchSongs")).toBe(false);
  });

  it("keeps loaded content as cached when a refresh times out, but not when the send is refused", async () => {
    let failWith: Error | null = null;
    const { bridge, snapshot, catalogCalls, emit } = createPreloadBridge({
      queryCatalog: (req) => {
        if (req.kind === "libraryPlaylists" && failWith) return Promise.reject(failWith);
        return new FakeCoreClient("normal").queryCatalog(req);
      },
    });
    renderApp(bridge, "library");
    await screen.findByTestId("playlist-card-mock-pl-favorites");
    const invalidate = (revision: number) =>
      emit({ deltaType: "resourceInvalidated", revision, resource_kind: "library" });

    failWith = new IpcTimeoutError("ui.catalogQuery");
    invalidate(snapshot.revision + 1);
    await waitFor(() => expect(catalogCalls.filter((r) => r.kind === "libraryPlaylists")).toHaveLength(2));
    await waitFor(() => expect(screen.getByTestId("library-state-cached").textContent).toContain("TIMEOUT"));
    expect(screen.getByTestId("playlist-card-mock-pl-favorites")).toBeTruthy();

    failWith = new Error("IPC rejected ui.catalogQuery");
    invalidate(snapshot.revision + 2);
    const errorBox = await screen.findByTestId("library-state-error");
    expect(errorBox.textContent).toContain("BRIDGE_INCOMPATIBLE");
    expect(errorBox.textContent).not.toMatch(/IPC|rejected/);
    expect(screen.queryByTestId("playlist-card-mock-pl-favorites")).toBeNull();
  });
});

describe("Catalog browsing", () => {
  function searchCatalog(req: CatalogQueryRequest): CatalogResponsePayload {
    if (req.kind === "searchSongs") {
      const second = req.cursor === "cur-1";
      return {
        resultType: "searchSongs",
        query: req.query ?? "",
        songs: paged(createSyntheticTracks(2, second ? "p2" : "p1"), second ? null : "cur-1"),
        albums: second ? [] : [{ id: "alb-1", browseId: null, title: "Album One" }],
        artists: second ? [] : [{ id: "art-1", name: "Artist One" }],
        playlists: [],
      };
    }
    if (req.kind === "albumDetail") {
      return {
        resultType: "albumDetail",
        albumId: req.albumId ?? "",
        title: "Album One",
        artists: [{ id: "art-1", name: "Artist One" }],
        tracks: createSyntheticTracks(3, "alb"),
        year: 2020,
        cover: null,
      };
    }
    if (req.kind === "artistDetail") {
      return {
        resultType: "artistDetail",
        artistId: req.artistId ?? "",
        name: "Artist One",
        topSongs: createSyntheticTracks(2, "art"),
        albums: [{ id: "alb-1", browseId: null, title: "Album One" }],
        cover: null,
      };
    }
    return new FakeCoreClient("normal").queryCatalog(req);
  }

  async function submitSearch(text: string) {
    const input = screen.getByTestId("search-input") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: text } });
    await fireEvent.submit(input.closest("form")!);
  }

  it("paginates search songs, shows album/artist sections, and navigates album → artist with Esc back", async () => {
    const { bridge, catalogCalls } = createPreloadBridge({ queryCatalog: searchCatalog });
    renderApp(bridge, "search");
    await waitFor(() => expect(screen.getByTestId("search-input")).toBeTruthy());

    await submitSearch("abc");
    await waitFor(() => {
      expect(screen.getByTestId("search-album-alb-1")).toBeTruthy();
      expect(screen.getByTestId("search-artist-art-1")).toBeTruthy();
      expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(2);
    });

    await fireEvent.click(screen.getByTestId("search-load-more"));
    await waitFor(() => expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(4));
    expect(catalogCalls.filter((r) => r.kind === "searchSongs").at(-1)).toMatchObject({ query: "abc", cursor: "cur-1" });
    expect(screen.queryByTestId("search-load-more")).toBeNull();

    await fireEvent.click(screen.getByTestId("search-album-alb-1"));
    await waitFor(() => {
      expect(screen.getByTestId("view-album")).toBeTruthy();
      expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(3);
    });
    await fireEvent.click(screen.getByTestId("album-artist-link"));
    await waitFor(() => {
      expect(screen.getByTestId("view-artist")).toBeTruthy();
      expect(screen.getByTestId("artist-album-alb-1")).toBeTruthy();
    });

    await fireEvent.keyDown(document.body, { key: "Escape" });
    await waitFor(() => expect(screen.getByTestId("view-album")).toBeTruthy());
    await fireEvent.keyDown(document.body, { key: "Escape" });
    await waitFor(() => expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(4));
  });

  it("navigates from a track's actions to its album and passes playlist context only from playlists", async () => {
    const { bridge } = createPreloadBridge({ queryCatalog: searchCatalog });
    renderApp(bridge, "library");
    await fireEvent.click(await screen.findByTestId("playlist-card-mock-pl-favorites"));
    await waitFor(() => expect(screen.getByTestId("view-playlist")).toBeTruthy());

    const rows = await screen.findAllByTestId("virtual-track-row");
    await fireEvent.click(rows[2]!.querySelector("[data-row-index]")!);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({
        op: "playTarget",
        args: { target: { videoId: "mock-vid-00003", playlistId: "mock-pl-favorites", startIndex: 2 } },
      })
    );

    await fireEvent.click(within(rows[0]!).getByTestId("track-actions-btn"));
    await fireEvent.click(await screen.findByTestId("action-go-album"));
    await waitFor(() => expect(screen.getByTestId("view-album")).toBeTruthy());
    const albumRows = await screen.findAllByTestId("virtual-track-row");
    await fireEvent.click(albumRows[0]!.querySelector("[data-row-index]")!);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ args: { target: { videoId: "alb-vid-00001", playlistId: null, startIndex: null } } })
    );
  });

  it("clears account-scoped results on account change and ignores late responses from the old account", async () => {
    let resolveLate: ((res: CatalogResponsePayload) => void) | null = null;
    const { bridge, snapshot, emit } = createPreloadBridge({
      queryCatalog: (req) => {
        if (req.kind === "searchSongs" && req.query === "late") {
          return new Promise<CatalogResponsePayload>((r) => (resolveLate = r));
        }
        return searchCatalog(req);
      },
    });
    renderApp(bridge, "search");
    await waitFor(() => expect(screen.getByTestId("search-input")).toBeTruthy());
    await submitSearch("first");
    await waitFor(() => expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(2));

    const switchAccount = (revision: number, epoch: string) =>
      emit({
        deltaType: "authChanged",
        revision,
        auth_state: { ...snapshot.authState, accountEpoch: epoch, accountScope: `scope-${epoch}` },
      });

    switchAccount(snapshot.revision + 1, "acct-2");
    await waitFor(() => {
      expect(screen.queryAllByTestId("virtual-track-row")).toHaveLength(0);
      expect((screen.getByTestId("search-input") as HTMLInputElement).value).toBe("");
    });

    await submitSearch("late");
    await waitFor(() => expect(resolveLate).not.toBeNull());
    switchAccount(snapshot.revision + 2, "acct-3");
    resolveLate!(searchCatalog({ kind: "searchSongs", query: "late" }));
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryAllByTestId("virtual-track-row")).toHaveLength(0);
    expect(screen.queryByTestId("search-album-alb-1")).toBeNull();
  });
});

describe("Account writes and queue controls", () => {
  it("drops Home and Recent history copies, including a late pre-deletion read", async () => {
    const fake = new FakeCoreClient("normal");
    const track = { ...createSyntheticTracks(1)[0]!, title: "Removed local history fixture" };
    const prior: CatalogResponsePayload = { resultType: "recentHistory", items: [track], freshness: "fresh" };
    let deleted = false;
    let recentReads = 0;
    let resolveOld: ((value: CatalogResponsePayload) => void) | null = null;
    const { bridge, emit, snapshot, catalogCalls } = createPreloadBridge({
      snapshot: writeReady,
      queryCatalog: (request) => {
        if (request.kind === "recentHistory") {
          recentReads += 1;
          if (recentReads === 2) return new Promise((resolve) => { resolveOld = resolve; });
          return { ...prior, items: deleted ? [] : [track] };
        }
        const result = fake.queryCatalog(request);
        return result.resultType === "home" ? { ...result, recentTracks: deleted ? [] : [track] } : result;
      },
    });
    renderApp(bridge);
    await screen.findByText(track.title);
    await fireEvent.click(screen.getByTestId("nav-recent"));
    await screen.findByText(track.title);
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "catalog" });
    await waitFor(() => expect(resolveOld).not.toBeNull());

    deleted = true;
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 2, resource_kind: "localHistory" });
    await waitFor(() => expect(recentReads).toBe(3));
    resolveOld!(prior);
    await tick();
    expect(screen.queryByText(track.title)).toBeNull();

    await fireEvent.click(screen.getByTestId("nav-home"));
    await waitFor(() => expect(catalogCalls.filter((request) => request.kind === "home")).toHaveLength(2));
    expect(screen.queryByText(track.title)).toBeNull();
  });

  it("refetches a playlist invalidated while offscreen, without reloading unrelated routes", async () => {
    const fake = new FakeCoreClient("normal");
    let title = "Playlist before remote edit";
    const { bridge, emit, snapshot, catalogCalls } = createPreloadBridge({
      queryCatalog: (req) => {
        const result = fake.queryCatalog(req);
        return result.resultType === "playlistTracks" ? { ...result, title } : result;
      },
    });
    renderApp(bridge, "library");
    const playlist = () => screen.findByTestId("playlist-card-mock-pl-favorites");
    await fireEvent.click(await playlist());
    await screen.findByText(title);
    await fireEvent.click(screen.getByTestId("nav-now-playing"));
    await screen.findByTestId("view-now-playing");

    emit({
      deltaType: "resourceInvalidated", revision: snapshot.revision + 1,
      resource_kind: "playlistTracks", resource_id: "another-playlist",
    });
    await fireEvent.click(screen.getByTestId("nav-library"));
    await fireEvent.click(await playlist());
    await screen.findByText(title);
    expect(catalogCalls.filter((req) => req.kind === "playlistTracks")).toHaveLength(1);
    await fireEvent.click(screen.getByTestId("nav-now-playing"));
    title = "Playlist after remote edit";
    emit({
      deltaType: "resourceInvalidated", revision: snapshot.revision + 2,
      resource_kind: "playlistTracks", resource_id: "mock-pl-favorites",
    });
    await tick();
    expect(catalogCalls.filter((req) => req.kind === "playlistTracks")).toHaveLength(1);
    await fireEvent.click(screen.getByTestId("nav-library"));
    await fireEvent.click(await playlist());
    await screen.findByText(title);
    expect(catalogCalls.filter((req) => req.kind === "playlistTracks")).toHaveLength(2);
    expect(catalogCalls.filter((req) => req.kind === "libraryPlaylists")).toHaveLength(1);
  });

  it("uses a refreshed liked collection without keeping an earlier local unlike", async () => {
    const sendMutation = vi.fn<NonNullable<YtmcpUiBridge["sendMutation"]>>(() => ({
      kind: "setLike", outcome: "confirmed", error: null,
    }));
    const { bridge, emit, snapshot } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    const button = await screen.findByTestId("np-like");
    await fireEvent.click(button);
    await waitFor(() => expect(button.getAttribute("aria-pressed")).toBe("false"));

    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "rating", resource_id: "mock-vid-00001" });
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 2, resource_kind: "likedSongs" });
    await fireEvent.click(screen.getByTestId("nav-liked"));
    await waitFor(() => expect(screen.queryByTestId("liked-state-loading")).toBeNull());
    await fireEvent.click(screen.getByTestId("nav-now-playing"));
    await waitFor(() => expect(screen.getByTestId("np-like").getAttribute("aria-pressed")).toBe("true"));
  });

  it("sends confirmed likes with the account epoch, reports indeterminate without retrying", async () => {
    const sendMutation = vi.fn(
      async (req: MutationRequest): Promise<MutationResult> =>
        req.kind === "setLike"
          ? { kind: "setLike", outcome: "indeterminate", error: { code: "TIMEOUT", retriable: false, retryAfterMs: null, safeMessage: "t", requestId: null } }
          : { kind: "addToPlaylist", outcome: "confirmed", error: null }
    );
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");

    const likeBtn = (await screen.findByTestId("np-like")) as HTMLButtonElement;
    expect(likeBtn.disabled).toBe(false);
    expect(likeBtn.textContent).toContain("Remove like"); // synthetic track #1 is liked
    await fireEvent.click(likeBtn);
    await waitFor(() => {
      const banner = screen.getByTestId("banner-mutation");
      expect(banner.getAttribute("data-tone")).toBe("unknown");
      expect(banner.textContent).toContain("Outcome unknown");
    });
    expect(sendMutation).toHaveBeenCalledTimes(1);
    expect(sendMutation).toHaveBeenCalledWith(
      { kind: "setLike", videoId: "mock-vid-00001", liked: false, playlistId: null, confirmedByUser: true },
      "mock-account-epoch-1"
    );
    expect(likeBtn.textContent).toContain("Remove like"); // unknown outcome is not applied

    await fireEvent.click(screen.getByTestId("np-actions"));
    await fireEvent.click(await screen.findByTestId("action-add-to-playlist"));
    await fireEvent.click(await screen.findByTestId("pick-playlist-mock-pl-late-night"));
    expect(sendMutation).toHaveBeenCalledTimes(1); // nothing sent before explicit confirm
    await fireEvent.click(await screen.findByTestId("confirm-add-to-playlist"));
    await waitFor(() => expect(screen.getByTestId("banner-mutation").getAttribute("data-tone")).toBe("success"));
    expect(sendMutation).toHaveBeenLastCalledWith(
      expect.objectContaining({ kind: "addToPlaylist", playlistId: "mock-pl-late-night", confirmedByUser: true }),
      "mock-account-epoch-1"
    );
  });

  it("disables writes that are unsupported, unverified or account-unmatched and explains why", async () => {
    const unverified = createPreloadBridge({
      snapshot: (s) => ({ ...s, capabilities: { ...s.capabilities, likeMutation: "unverified" } }),
      sendMutation: vi.fn(),
    });
    renderApp(unverified.bridge, "now-playing");
    const like = (await screen.findByTestId("np-like")) as HTMLButtonElement;
    expect(like.disabled).toBe(true);
    expect(screen.getByTestId("view-now-playing").textContent).toContain("This change is not verified yet");
    cleanup();

    const unmatched = createPreloadBridge({
      snapshot: (s) => ({
        ...writeReady(s),
        capabilities: { ...writeReady(s).capabilities, queueEnqueue: "unsupported" },
        authState: { ...s.authState, playerCatalogMatched: false },
      }),
    });
    renderApp(unmatched.bridge, "now-playing");
    expect(((await screen.findByTestId("np-like")) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("view-now-playing").textContent).toContain("not confirmed to match");

    await fireEvent.click(screen.getByTestId("np-actions"));
    const actions = await screen.findByTestId("track-actions");
    expect((within(actions).getByTestId("action-play-next") as HTMLButtonElement).disabled).toBe(true);
    expect(actions.textContent).toContain("This connection does not support this action");
  });

  it("enqueues next with the queue revision and gates shuffle/repeat by capability", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => ({ ...writeReady(s), capabilities: { ...writeReady(s).capabilities, shuffle: "supported", repeat: "unverified" } }),
    });
    renderApp(bridge, "queue");
    const rows = await screen.findAllByTestId("virtual-track-row");
    await fireEvent.click(within(rows[1]!).getByTestId("track-actions-btn"));
    await fireEvent.click(await screen.findByTestId("action-play-next"));
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({
        op: "enqueue",
        args: { target: { videoId: "mock-vid-00002", playlistId: null, startIndex: null }, enqueuePosition: "next", queueRevision: 1 },
      })
    );

    await fireEvent.click(screen.getByTestId("btn-shuffle"));
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "setShuffle", args: { shuffle: true } }));
    const repeat = screen.getByTestId("btn-repeat") as HTMLButtonElement;
    expect(repeat.disabled).toBe(false);
    expect(repeat.title).toContain("not verified yet");
    await fireEvent.click(repeat);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "setRepeat", args: { repeat: "all" } }));

    await fireEvent.click(rows[2]!.querySelector("[data-row-index]")!);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: "selectQueueEntry", args: { queueEntryId: "qe-mock-3", queueRevision: 1 } })
    );
  });
});

describe("Player bar seek preview", () => {
  it("keeps the preview until the seek receipt settles and rolls back on failure or track change", async () => {
    const fake = new FakeCoreClient("normal");
    const snap = fake.subscribeSnapshot(() => {}).snapshot;
    let resolveCmd: ((r: CommandReceipt | null) => void) | null = null;
    const onCommand = vi.fn(() => new Promise<CommandReceipt | null>((r) => (resolveCmd = r)));
    const overlay = { pendingCount: 0, activeOp: null, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };
    const { rerender } = render(PlayerBar, {
      props: { snapshot: snap, pendingOverlay: overlay, interpolatedPositionMs: 12_000, onCommand, onOpenNowPlaying: () => {}, onOpenLyrics: () => {}, onOpenDiagnostics: () => {} },
    });

    const slider = screen.getByTestId("player-seek-slider") as HTMLInputElement;
    await fireEvent.input(slider, { target: { value: "50000" } });
    // The preview is the elapsed time itself, in the accent: no extra label appears.
    expect(screen.getByTestId("player-pos-text").textContent).toBe("0:50");
    expect(screen.getByTestId("player-pos-text").classList.contains("is-previewing")).toBe(true);
    await fireEvent.change(slider, { target: { value: "50000" } });
    expect(onCommand).toHaveBeenCalledWith("seek", { positionMs: 50_000, trackInstanceId: "ti-mock-1" });
    resolveCmd!({ commandId: "seek-1", op: "seek", status: "accepted", acceptedAtMs: 0, observedAtMs: null, deduplicated: false, error: null });
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.getByTestId("player-pos-text").textContent).toBe("0:50");

    const failed: CommandReceipt = { commandId: "seek-1", op: "seek", status: "failed", acceptedAtMs: 0, observedAtMs: null, deduplicated: false, error: null };
    await rerender({ snapshot: { ...snap, pending: { ...snap.pending, lastReceipt: failed } } });
    await waitFor(() => expect(screen.getByTestId("player-pos-text").classList.contains("is-previewing")).toBe(false));
    expect(screen.getByTestId("player-pos-text").textContent).toBe("0:12");

    await fireEvent.input(slider, { target: { value: "90000" } });
    expect(screen.getByTestId("player-pos-text").classList.contains("is-previewing")).toBe(true);
    await rerender({ snapshot: { ...snap, player: { ...snap.player, trackInstanceId: "ti-mock-2" } } });
    await waitFor(() => expect(screen.getByTestId("player-pos-text").classList.contains("is-previewing")).toBe(false));
  });

  it("changes volume without any words appearing in the bar and holds the dragged value", async () => {
    const fake = new FakeCoreClient("normal");
    const snap = fake.subscribeSnapshot(() => {}).snapshot;
    const onCommand = vi.fn(() => new Promise<CommandReceipt | null>(() => {}));
    const pendingVolume = { pendingCount: 1, activeOp: "setVolume" as const, desiredPlayback: null, desiredPositionMs: null, desiredVolume: 0.3, lastError: null };
    render(PlayerBar, {
      props: { snapshot: snap, pendingOverlay: pendingVolume, interpolatedPositionMs: 12_000, onCommand, onOpenNowPlaying: () => {}, onOpenLyrics: () => {}, onOpenDiagnostics: () => {} },
    });
    const bar = screen.getByTestId("player-bar");
    const before = bar.textContent;
    const slider = screen.getByTestId("player-volume-slider") as HTMLInputElement;

    await fireEvent.pointerDown(slider);
    await fireEvent.input(slider, { target: { value: "30" } });
    expect(onCommand).toHaveBeenLastCalledWith("setVolume", { volume: 0.3 });
    // Only the number on the mute button changes; nothing like "waiting" is added.
    expect(bar.textContent).toBe(before!.replace(`${Math.round(snap.player.volume * 100)}%`, "30%"));
    expect(slider.value).toBe("30");
    expect(slider.getAttribute("title")).toBeNull();
    expect(screen.getByTestId("player-observed-status").classList.contains("is-waiting")).toBe(false);
  });
});

describe("Covers, lists and text safety", () => {
  it("uses app://covers for cached assets, a neutral placeholder otherwise, and never remote URLs", () => {
    const tracks = [
      { ...LONG_CJK_MIXED_SAMPLE_TRACK, videoId: "a", cover: { assetId: "abc123", remoteUrl: "https://example.com/a.jpg" } },
      { ...LONG_CJK_MIXED_SAMPLE_TRACK, videoId: "b", cover: { assetId: null, remoteUrl: "https://example.com/b.jpg" } },
    ];
    const { container } = render(VirtualizedTrackList, { props: { tracks, mockMode: false, onPlayTrack: () => {} } });
    const srcs = Array.from(container.querySelectorAll("img")).map((img) => img.getAttribute("src"));
    expect(srcs).toEqual(["app://covers/abc123"]);
    expect(container.querySelectorAll("[data-testid='cover-placeholder']")).toHaveLength(1);
    expect(container.textContent).toContain("深夜城市玻璃交响曲");
  });

  it("virtualizes 10,000 tracks with bounded DOM rows and unique keys for duplicate queue videoIds", async () => {
    const { getAllByTestId } = render(VirtualizedTrackList, {
      props: { tracks: createSyntheticTracks(10_000, "test10k"), mockMode: true, heightPx: 480, onPlayTrack: () => {} },
    });
    const renderedRows = getAllByTestId("virtual-track-row");
    expect(renderedRows.length).toBeGreaterThan(0);
    expect(renderedRows.length).toBeLessThan(60);
    cleanup();

    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }), initialView: "queue" } });
    await waitFor(() => expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(3));
    const keys = screen.getAllByTestId("virtual-track-row").map((el) => el.getAttribute("data-row-key"));
    expect(keys).toEqual(["qe-mock-1", "qe-mock-2", "qe-mock-3"]);
  });

  it("renders lyrics as plain text, seeks to offset-adjusted line time, and exposes no seek for plain lyrics", async () => {
    const xss = '<img src=x onerror="window.__xss=1"><b>Injected</b>';
    const synced: LyricsResult = {
      ...DISABLED_LYRICS,
      kind: "synced",
      source: "lrclib",
      fileOffsetMs: 200,
      userOffsetMs: 300,
      syncedLines: [{ timeMs: 1000, text: xss }],
    };
    const { bridge } = createPreloadBridge({ queryLyrics: () => synced });
    renderApp(bridge, "lyrics");
    const list = await screen.findByTestId("lyrics-synced-list");
    expect(list.querySelector("img")).toBeNull();
    expect(list.querySelector("b")).toBeNull();
    expect(list.textContent).toContain(xss);
    await fireEvent.click(screen.getByTestId("lyrics-seek-line"));
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: "seek", args: { positionMs: 1_100, trackInstanceId: "ti-mock-1" } })
    );
    cleanup();

    const plain = createPreloadBridge({ queryLyrics: () => ({ ...synced, kind: "plain", syncedLines: [], plainText: `First\n${xss}` }) });
    renderApp(plain.bridge, "lyrics");
    const box = await screen.findByTestId("lyrics-plain-text");
    expect(screen.queryAllByTestId("lyrics-seek-line")).toHaveLength(0);
    expect(box.querySelector("b")).toBeNull();
    expect(box.textContent).toContain(xss);
  });

  it("refreshes same-track lyrics after a cache clear and drops the older reply", async () => {
    const plain = (text: string): LyricsResult => ({
      ...DISABLED_LYRICS, kind: "plain", source: "lrclib", plainText: text,
    });
    let resolveOld!: (value: LyricsResult) => void;
    const queryLyrics = vi.fn()
      .mockImplementationOnce(() => new Promise<LyricsResult>((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce(plain("Fresh lyrics"))
      .mockResolvedValueOnce(plain("Refreshed while visible"));
    const { bridge, emit, snapshot } = createPreloadBridge({ queryLyrics });
    renderApp(bridge, "lyrics");
    await waitFor(() => expect(queryLyrics).toHaveBeenCalledTimes(1));

    await fireEvent.click(screen.getByTestId("nav-settings"));
    await fireEvent.click(await screen.findByTestId("settings-clear-cache"));
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "catalog" });
    await tick();
    expect(queryLyrics).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByTestId("nav-lyrics"));
    await screen.findByText("Fresh lyrics");
    resolveOld(plain("Stale lyrics"));
    await tick();
    expect(screen.getByTestId("lyrics-plain-text").textContent).toBe("Fresh lyrics");

    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 2, resource_kind: "likedSongs" });
    await tick();
    expect(queryLyrics).toHaveBeenCalledTimes(2);
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 3, resource_kind: "catalog" });
    await screen.findByText("Refreshed while visible");
    expect(queryLyrics).toHaveBeenCalledTimes(3);
  });

  it("refetches the offset after another client saves one and steps from the new value", async () => {
    const synced = (userOffsetMs: number): LyricsResult => ({
      ...DISABLED_LYRICS,
      kind: "synced",
      source: "lrclib",
      userOffsetMs,
      syncedLines: [{ timeMs: 1000, text: "Fixture line" }],
    });
    const queryLyrics = vi.fn()
      .mockResolvedValueOnce(synced(0))
      .mockResolvedValueOnce(synced(2_000))
      .mockResolvedValue(synced(2_500));
    const { bridge, emit, snapshot } = createPreloadBridge({ queryLyrics });
    const videoId = snapshot.player.currentTrack!.videoId;
    renderApp(bridge, "lyrics");
    await screen.findByTestId("lyrics-synced-list");

    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "lyricsOffset", resource_id: `${videoId}-other` });
    await tick();
    expect(queryLyrics).toHaveBeenCalledTimes(1);
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 2, resource_kind: "lyricsOffset", resource_id: videoId });
    await waitFor(() => expect(queryLyrics).toHaveBeenCalledTimes(2));
    expect(queryLyrics).toHaveBeenLastCalledWith(expect.objectContaining({ setUserOffsetMs: null, resetUserOffset: false }));
    await tick();
    await fireEvent.click(screen.getByRole("button", { name: en.lyrics.offsetLater }));
    expect(queryLyrics).toHaveBeenLastCalledWith(expect.objectContaining({ setUserOffsetMs: 2_500 }));
  });

  it("highlights calibrated lyrics within 300 ms through play, seek, pause and resume", async () => {
    const lineTimes = Array.from({ length: 20 }, (_, index) => 241 + index * 257);
    const lyrics: LyricsResult = {
      ...DISABLED_LYRICS,
      kind: "synced",
      source: "lrclib",
      fileOffsetMs: 200,
      userOffsetMs: 300,
      syncedLines: lineTimes.map((time, index) => ({ timeMs: time - 100, text: `Fixture line ${index}` })),
    };
    const { bridge, emit, snapshot } = createPreloadBridge({ queryLyrics: () => lyrics });
    renderApp(bridge, "lyrics");
    const list = await screen.findByTestId("lyrics-synced-list");
    const active = () => list.querySelector("[aria-current=true]")?.getAttribute("data-line-index");
    let revision = snapshot.revision;
    const anchor = async (positionMs: number, playing: boolean) => {
      emit({
        deltaType: "playerChanged",
        revision: ++revision,
        pending: snapshot.pending,
        player: {
          ...snapshot.player,
          playback: playing ? "playing" : "paused",
          timeAnchor: { ...snapshot.player.timeAnchor, positionMs, playbackRate: 1, extrapolating: playing },
        },
      });
      await tick();
    };

    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "performance"] });
    try {
      await anchor(0, true);
      const displays = new Map<number, number>();
      for (let ms = 20; ms <= 5300; ms += 20) {
        await vi.advanceTimersByTimeAsync(20);
        const line = active();
        if (line !== undefined && !displays.has(Number(line))) displays.set(Number(line), ms);
      }
      expect(displays.size).toBe(lineTimes.length);
      const delays = lineTimes.map((time, index) => displays.get(index)! - time).sort((a, b) => a - b);
      expect(delays[0]).toBeGreaterThanOrEqual(0);
      expect(delays[Math.ceil(delays.length * 0.95) - 1]).toBeLessThanOrEqual(300);

      await fireEvent.click(screen.getAllByTestId("lyrics-seek-line")[4]!);
      expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({
        op: "seek",
        args: { positionMs: lineTimes[4], trackInstanceId: "ti-mock-1" },
      }));
      await anchor(lineTimes[4]!, false);
      await vi.advanceTimersByTimeAsync(600);
      expect(active()).toBe("4");
      await anchor(lineTimes[4]!, true);
      await vi.advanceTimersByTimeAsync(400);
      expect(active()).toBe("5");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("Keyboard, dialogs and settings", () => {
  it("does not trigger Space playback in inputs or during IME composition", async () => {
    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }), initialView: "search" } });
    const status = () => screen.getByTestId("player-observed-status").textContent?.trim();
    await waitFor(() => expect(screen.getByTestId("search-input")).toBeTruthy());
    expect(status()).toBe("Paused");

    const input = screen.getByTestId("search-input") as HTMLInputElement;
    input.focus();
    await fireEvent.keyDown(input, { key: " ", code: "Space" });
    expect(status()).toBe("Paused");

    input.blur();
    await fireEvent(window, new CompositionEvent("compositionstart", { data: "ni" }));
    await fireEvent.keyDown(document.body, { key: " ", code: "Space", isComposing: true });
    expect(status()).toBe("Paused");

    await fireEvent(window, new CompositionEvent("compositionend", { data: "你" }) /* pinyin "ni" committed */);
    await fireEvent.keyDown(document.body, { key: " ", code: "Space" });
    await waitFor(() => expect(status()).toBe("Playing"));
  });

  it("handles Ctrl+F, Ctrl+L and Ctrl+Arrow shortcuts without hijacking text inputs", async () => {
    const { bridge } = createPreloadBridge();
    renderApp(bridge, "home");
    await waitFor(() => expect(screen.getByTestId("view-home")).toBeTruthy());

    await fireEvent.keyDown(document.body, { key: "ArrowRight", ctrlKey: true });
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "next" }));

    await fireEvent.keyDown(document.body, { key: "l", ctrlKey: true });
    await waitFor(() => expect(screen.getByTestId("view-lyrics")).toBeTruthy());

    await fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
    await waitFor(() => expect(document.activeElement?.id).toBe("ytm-search-input"));
    const calls = bridge.sendPlayerCommand.mock.calls.length;
    await fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft", ctrlKey: true });
    expect(bridge.sendPlayerCommand.mock.calls.length).toBe(calls);
  });

  it("switches the whole UI to Chinese from Settings through the settings update", async () => {
    const fake = new FakeCoreClient("normal");
    render(App, { props: { client: createUiClient({ fakeClient: fake }), initialView: "settings" } });
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeTruthy();
    expect(document.documentElement.lang).toBe("en");

    await fireEvent.change(screen.getByTestId("setting-language-select"), { target: { value: "zh-CN" } });
    expect(await screen.findByRole("heading", { name: "设置" })).toBeTruthy();
    // Already-rendered chrome outside the view follows too, including derived labels.
    expect(screen.getByTestId("nav-home").textContent).toContain("首页");
    expect(screen.getByTestId("player-observed-status").textContent?.trim()).toBe("已暂停");
    expect(screen.getByTestId("settings-account-state").textContent).toContain("登录状态：已登录");
    expect(document.documentElement.lang).toBe("zh-CN");
    expect(fake.subscribeSnapshot(() => {}).snapshot.settings.uiLanguage).toBe("zh-CN");
  });

  it("takes the UI language from the settings snapshot and changes it only once core confirms", async () => {
    const { bridge, snapshot, emit } = createPreloadBridge({
      snapshot: (s) => ({ ...s, settings: { ...s.settings, uiLanguage: "zh-CN" } }),
    });
    renderApp(bridge, "settings");
    expect(await screen.findByRole("heading", { name: "设置" })).toBeTruthy();
    expect(document.documentElement.lang).toBe("zh-CN");
    const select = screen.getByTestId("setting-language-select") as HTMLSelectElement;
    expect(select.value).toBe("zh-CN");

    await fireEvent.change(select, { target: { value: "en" } });
    expect(bridge.updateSettings).toHaveBeenLastCalledWith({ uiLanguage: "en" });
    expect(screen.getByRole("heading", { name: "设置" })).toBeTruthy();

    emit({
      deltaType: "settingsChanged",
      revision: snapshot.revision + 1,
      settings: { ...snapshot.settings, uiLanguage: "en" },
    });
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeTruthy();
    expect(document.documentElement.lang).toBe("en");
  });

  it("traps focus in the diagnostics dialog and restores it on Esc", async () => {
    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }) } });
    const trigger = (await screen.findByTestId("btn-open-diagnostics")) as HTMLButtonElement;
    trigger.focus();
    await fireEvent.click(trigger);
    const dialog = await screen.findByTestId("accessible-dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
    await fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("accessible-dialog")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  });

  it("disables close-to-tray without a tray, has no inert toggles, and logs out only after confirm", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => ({ ...s, hostHealth: { ...s.hostHealth, trayAvailable: false } }),
    });
    renderApp(bridge, "settings");
    const tray = (await screen.findByTestId("setting-close-to-tray")) as HTMLInputElement;
    expect(tray.disabled).toBe(true);
    expect(tray.checked).toBe(false);
    expect(screen.getByTestId("tray-unavailable-note")).toBeTruthy();
    expect(screen.getByTestId("view-settings").textContent).not.toMatch(/native window/i);
    const notifications = screen.getByTestId("setting-track-notifications") as HTMLInputElement;
    expect(notifications.checked).toBe(false);
    await fireEvent.click(notifications);
    expect(bridge.updateSettings).toHaveBeenLastCalledWith({ trackNotifications: true });

    await fireEvent.click(screen.getByTestId("settings-logout"));
    await screen.findByTestId("accessible-dialog");
    expect(bridge.logout).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByTestId("confirm-logout"));
    expect(bridge.logout).toHaveBeenCalledTimes(1);
  });

  it("offers only the UI scales the host applies and shows a larger stored scale as 150%", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => ({ ...s, settings: { ...s.settings, uiScalePercent: 200 } }),
    });
    renderApp(bridge, "settings");
    const scale = (await screen.findByTestId("setting-ui-scale")) as HTMLSelectElement;
    expect([...scale.options].map((option) => option.value)).toEqual(["100", "125", "150"]);
    expect(scale.value).toBe("150");
  });

  it("sends only the changed field patch and shows feedback + reverts pending toggle on save failure", async () => {
    const { bridge, snapshot } = createPreloadBridge();
    const base = snapshot.settings;
    renderApp(bridge, "settings");
    const motion = (await screen.findByTestId("setting-reduce-motion")) as HTMLInputElement;
    await fireEvent.click(motion);
    expect(bridge.updateSettings).toHaveBeenLastCalledWith({ reduceMotion: !base.reduceMotion });
    await fireEvent.click(screen.getByTestId("setting-lrclib"));
    expect(bridge.updateSettings).toHaveBeenLastCalledWith({ lrclibEnabled: !base.lrclibEnabled });

    bridge.updateSettings.mockRejectedValueOnce(
      new CoreRequestError({
        code: "STORAGE_ERROR",
        retriable: false,
        retryAfterMs: null,
        safeMessage: "settings disk read-only",
        requestId: null,
      })
    );
    const autoplay = screen.getByTestId("setting-autoplay") as HTMLInputElement;
    expect(autoplay.checked).toBe(base.autoplayOnLaunch);
    await fireEvent.click(autoplay);
    expect((await screen.findByTestId("settings-save-error")).textContent).toContain("settings disk read-only");
    await waitFor(() => expect(autoplay.checked).toBe(base.autoplayOnLaunch));
  });

  it("wires official player open, account match status, cache clear feedback, search clear, now-playing seek, banner dismiss and localized diagnostics", async () => {
    const failedReceipt: CommandReceipt = {
      commandId: "cmd-fail-1",
      op: "play",
      status: "failed",
      acceptedAtMs: 0,
      observedAtMs: null,
      deduplicated: false,
      error: { code: "TIMEOUT", retriable: false, retryAfterMs: null, safeMessage: "player timed out", requestId: null },
    };
    const { bridge, emit, snapshot } = createPreloadBridge({
      snapshot: (s) => ({
        ...writeReady(s),
        pending: { ...s.pending, lastReceipt: failedReceipt },
      }),
    });
    renderApp(bridge, "settings");

    expect((await screen.findByTestId("settings-account-match")).textContent).toContain("accounts match");
    await fireEvent.click(screen.getByTestId("settings-open-official-player"));
    expect(bridge.openPlayerWindow).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByTestId("settings-clear-cache"));
    expect(bridge.clearCache).toHaveBeenCalledTimes(1);
    expect(await screen.findByTestId("settings-cache-pending")).toBeTruthy();
    expect(screen.queryByTestId("settings-cache-cleared")).toBeNull();
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "catalog" });
    expect(await screen.findByTestId("settings-cache-cleared")).toBeTruthy();

    expect(screen.getByTestId("banner-command-error")).toBeTruthy();
    await fireEvent.click(screen.getByTestId("dismiss-command-banner"));
    await waitFor(() => expect(screen.queryByTestId("banner-command-error")).toBeNull());

    await fireEvent.click(screen.getByTestId("settings-open-diagnostics"));
    const diag = await screen.findByTestId("diagnostics-content");
    expect(diag.textContent).toContain("GPU compositing");
    expect(diag.textContent).toContain("MPRIS media controls");
    expect(diag.textContent).toContain("Play / pause");
    expect(diag.textContent).toContain("Supported");
    await fireEvent.keyDown(screen.getByTestId("accessible-dialog"), { key: "Escape" });

    await fireEvent.click(screen.getByTestId("nav-now-playing"));
    const npSlider = (await screen.findByTestId("np-seek-slider")) as HTMLInputElement;
    await fireEvent.change(npSlider, { target: { value: "42000" } });
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: "seek", args: { positionMs: 42_000, trackInstanceId: "ti-mock-1" } })
    );

    await fireEvent.click(screen.getByTestId("nav-search"));
    const input = (await screen.findByTestId("search-input")) as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "Neon" } });
    await fireEvent.submit(input.closest("form")!);
    await waitFor(() => expect(screen.getAllByTestId("virtual-track-row").length).toBeGreaterThan(0));
    await fireEvent.click(screen.getByTestId("search-clear-btn"));
    await waitFor(() => {
      expect(input.value).toBe("");
      expect(screen.queryAllByTestId("virtual-track-row")).toHaveLength(0);
    });
  });
});

describe("Appearance and glass shell", () => {
  it("writes the resolved colour scheme, transparency setting and sidebar state for the stylesheet", async () => {
    // The stylesheet has one light palette keyed on data-theme="light"; "system" is resolved here.
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query === "(prefers-color-scheme: light)",
      addEventListener: () => {},
      removeEventListener: () => {},
    }));
    try {
      const { bridge } = createPreloadBridge({
        snapshot: (s) => ({ ...s, settings: { ...s.settings, theme: "system", reduceTransparency: true } }),
      });
      const { container } = renderApp(bridge);
      await waitFor(() => expect(document.documentElement.dataset.theme).toBe("light"));
      expect(document.documentElement.dataset.reduceTransparency).toBe("true");

      const shell = container.querySelector(".app-shell")!;
      expect(shell.getAttribute("data-sidebar")).toBe("expanded");
      await fireEvent.click(screen.getByTestId("sidebar-collapse-btn"));
      expect(shell.getAttribute("data-sidebar")).toBe("collapsed");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("puts glass only on the sidebar, the player bar and dialogs, and keeps list text off the bleed layers", async () => {
    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }) } });
    await screen.findByTestId("home-playlist-mock-pl-favorites");
    const glass = () => Array.from(document.querySelectorAll(".lg")).map((el) => el.getAttribute("data-testid"));
    expect(glass()).toEqual(["app-sidebar", "player-bar"]);
    await fireEvent.click(screen.getByTestId("btn-open-diagnostics"));
    await screen.findByTestId("accessible-dialog");
    expect(glass()).toEqual(["app-sidebar", "player-bar", "accessible-dialog"]);

    // Only the hero wash and the cover shelf leave the page column; track rows never do.
    expect(screen.getByTestId("cover-shelf").classList.contains("bleed")).toBe(true);
    expect(screen.getByTestId("hero-wash").closest(".page-hero")).not.toBeNull();
    for (const row of screen.getAllByTestId("virtual-track-row")) expect(row.closest(".bleed, .hero-wash")).toBeNull();
  });
});

describe("Raw preload envelope bridge", () => {
  it("bridges subscribeSnapshot, deltas and playerCommand over envelopes", async () => {
    const fake = new FakeCoreClient("normal");
    const listeners = new Set<(env: Envelope) => void>();
    const rawPreload: RawPreloadUiBridge = {
      onEnvelope(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      async sendEnvelope(env) {
        const emit = (kind: Envelope["kind"], payload: unknown, requestId?: string | null) => {
          const out: Envelope = { protocolVersion: PROTOCOL_VERSION, kind, coreEpoch: "mock-core-epoch-1", requestId: requestId ?? null, seq: env.seq, payload };
          for (const l of listeners) l(out);
        };
        if (env.kind === "ui.subscribeSnapshot") {
          const sub = fake.subscribeSnapshot((delta) => emit("core.delta", delta));
          emit("core.snapshot", sub.snapshot, env.requestId);
        } else if (env.kind === "ui.playerCommand") {
          emit("core.commandReceipt", fake.sendPlayerCommand(env.payload as never), env.requestId);
        }
        return { ok: true };
      },
    };
    const client = createUiClient({ preloadBridge: wrapPreloadEnvelopeBridge(rawPreload) });
    await client.connect();
    expect(client.getState().snapshot?.coreEpoch).toBe("mock-core-epoch-1");
    const receipt = await client.sendPlayerCommand("play");
    expect(receipt?.status).toBe("observed");
    expect(client.getState().snapshot?.player.playback).toBe("playing");
  });
});

describe("Queue editing, play gating, cache clear and album context", () => {
  /** A complete five-entry queue: one history entry, the current one, three upcoming. */
  function editableQueue(s: AppSnapshot, overrides: Partial<AppSnapshot["queueSummary"]> = {}): AppSnapshot {
    const tracks = createSyntheticTracks(5, "q");
    const ids = ["qe-h", "qe-cur", "qe-b", "qe-c", "qe-d"];
    const previewEntries = tracks.map((track, i) => ({ entryId: ids[i]!, videoId: track.videoId, track, availability: "available" as const }));
    return {
      ...s,
      capabilities: { ...s.capabilities, queueRemove: "supported", queueReorder: "supported", queueAutoplay: "supported", queueClear: "supported" },
      player: { ...s.player, currentQueueEntryId: "qe-cur", trackInstanceId: "ti-q", autoplay: true },
      queueSummary: {
        queueRevision: 7,
        currentEntryId: "qe-cur",
        completeness: "complete",
        hasMore: false,
        loadedCount: previewEntries.length,
        previewEntries,
        ...overrides,
      },
    };
  }

  const accepted = (cmd: { commandId: string; op: CommandReceipt["op"] }): CommandReceipt => ({
    commandId: cmd.commandId, op: cmd.op, status: "accepted", acceptedAtMs: 0, observedAtMs: null, deduplicated: false, error: null,
  });

  async function queueRows() {
    const rows = await screen.findAllByTestId("virtual-track-row");
    expect(rows).toHaveLength(5);
    return rows;
  }
  const button = (row: HTMLElement, id: string) => within(row).getByTestId(id) as HTMLButtonElement;

  it("keeps clear upcoming disabled while the page refuses clearing, single edits still work", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => {
        const queue = editableQueue(s);
        return { ...queue, capabilities: { ...queue.capabilities, queueClear: "unsupported" } };
      },
    });
    renderApp(bridge, "queue");
    const rows = await queueRows();
    const clear = screen.getByTestId("queue-clear-upcoming") as HTMLButtonElement;
    expect(clear.disabled).toBe(true);
    expect(clear.title).toBe(en.queue.clearUnavailable);
    expect(button(rows[2]!, "queue-remove").disabled).toBe(false);
  });

  it("sends exact remove, move and clear commands only for upcoming occurrences", async () => {
    const { bridge } = createPreloadBridge({ snapshot: (s) => editableQueue(s) });
    bridge.sendPlayerCommand.mockImplementation((cmd) => ({ ...accepted(cmd), status: "failed" }) as never);
    renderApp(bridge, "queue");
    const rows = await queueRows();

    expect(within(rows[0]!).queryByTestId("queue-row-actions")).toBeNull();
    expect(within(rows[1]!).queryByTestId("queue-row-actions")).toBeNull();
    expect(button(rows[2]!, "queue-move-up").disabled).toBe(true);
    expect(button(rows[4]!, "queue-move-down").disabled).toBe(true);

    const sent = () => {
      const cmd = bridge.sendPlayerCommand.mock.lastCall![0];
      return [cmd.op, cmd.args];
    };
    await fireEvent.click(button(rows[2]!, "queue-move-down"));
    expect(sent()).toEqual(["moveQueueEntry", { queueEntryId: "qe-b", queueRevision: 7, beforeQueueEntryId: "qe-d" }]);
    await fireEvent.click(button(rows[3]!, "queue-move-up"));
    expect(sent()).toEqual(["moveQueueEntry", { queueEntryId: "qe-c", queueRevision: 7, beforeQueueEntryId: "qe-b" }]);
    await fireEvent.click(button(rows[3]!, "queue-move-down"));
    expect(sent()).toEqual(["moveQueueEntry", { queueEntryId: "qe-c", queueRevision: 7 }]);
    await fireEvent.click(button(rows[3]!, "queue-remove"));
    expect(sent()).toEqual(["removeQueueEntry", { queueEntryId: "qe-c", queueRevision: 7 }]);
    await fireEvent.click(screen.getByTestId("queue-clear-upcoming"));
    expect(sent()).toEqual(["clearUpcoming", { queueEntryId: "qe-cur", queueRevision: 7, trackInstanceId: "ti-q" }]);
    await fireEvent.click(screen.getByTestId("queue-autoplay-toggle"));
    expect(sent()).toEqual(["setAutoplay", { autoplay: false, queueRevision: 7 }]);
  });

  it("keeps a row pending until its receipt settles and surfaces the failure", async () => {
    const { bridge, emit, snapshot } = createPreloadBridge({ snapshot: (s) => editableQueue(s) });
    bridge.sendPlayerCommand.mockImplementation((cmd) => accepted(cmd) as never);
    renderApp(bridge, "queue");
    const rows = await queueRows();

    await fireEvent.click(button(rows[3]!, "queue-remove"));
    await screen.findByTestId("queue-row-pending");
    expect(button(rows[2]!, "queue-remove").disabled).toBe(true);
    expect((screen.getByTestId("queue-clear-upcoming") as HTMLButtonElement).disabled).toBe(true);

    const cmd = bridge.sendPlayerCommand.mock.lastCall![0];
    emit({
      deltaType: "commandUpdated",
      revision: snapshot.revision + 1,
      receipt: {
        ...accepted(cmd),
        status: "failed",
        error: { code: "STALE_CONTEXT", retriable: false, retryAfterMs: null, safeMessage: "queue changed", requestId: null },
      },
    });
    await waitFor(() => expect(screen.queryByTestId("queue-row-pending")).toBeNull());
    expect(button(rows[2]!, "queue-remove").disabled).toBe(false);
    expect(screen.getByTestId("banner-command-error").textContent).toContain("queue changed");
  });

  it("disables edits on partial, truncated or unanchored queues and autoplay when unknown", async () => {
    for (const snap of [
      (s: AppSnapshot) => editableQueue(s, { completeness: "partial" }),
      (s: AppSnapshot) => editableQueue(s, { hasMore: null }),
      (s: AppSnapshot) => editableQueue(s, { loadedCount: 120 }),
      (s: AppSnapshot) => ({ ...editableQueue(s), player: { ...editableQueue(s).player, trackInstanceId: null, autoplay: null } }),
    ]) {
      const { bridge } = createPreloadBridge({ snapshot: snap });
      renderApp(bridge, "queue");
      const rows = await queueRows();
      for (const id of ["queue-move-up", "queue-move-down", "queue-remove"]) {
        expect(button(rows[3]!, id).disabled).toBe(true);
      }
      expect((screen.getByTestId("queue-clear-upcoming") as HTMLButtonElement).disabled).toBe(true);
      expect(screen.getByTestId("queue-note").textContent).toMatch(/fully loaded|current track/);
      cleanup();
    }
    const { bridge } = createPreloadBridge({
      snapshot: (s) => ({ ...editableQueue(s), player: { ...editableQueue(s).player, autoplay: null } }),
    });
    renderApp(bridge, "queue");
    expect(((await screen.findByTestId("queue-autoplay-toggle")) as HTMLButtonElement).disabled).toBe(true);
  });

  it("disables play without a loaded track but keeps pause available", async () => {
    const fake = new FakeCoreClient("normal");
    const snap = fake.subscribeSnapshot(() => {}).snapshot;
    const overlay = { pendingCount: 0, activeOp: null, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };
    const props = { pendingOverlay: overlay, interpolatedPositionMs: 0, onCommand: vi.fn(async () => null), onOpenNowPlaying: () => {}, onOpenLyrics: () => {}, onOpenDiagnostics: () => {} };
    const { rerender } = render(PlayerBar, { props: { ...props, snapshot: { ...snap, player: { ...snap.player, currentTrack: null, playback: "paused" } } } });
    const play = screen.getByTestId("btn-play-pause") as HTMLButtonElement;
    expect(play.disabled).toBe(true);
    expect(play.title).toContain("No track is loaded");

    await rerender({ snapshot: { ...snap, player: { ...snap.player, currentTrack: null, playback: "playing" } } });
    expect(play.disabled).toBe(false);
    await rerender({ snapshot: snap });
    expect(play.disabled).toBe(false);
  });

  it("reports cache clear success only on core's catalog invalidation, failure on errors, and unconfirmed on timeout", async () => {
    const { bridge } = createPreloadBridge();
    bridge.clearCache.mockImplementation(() => {
      throw new CoreRequestError({ code: "BUSY", retriable: true, retryAfterMs: null, safeMessage: "core busy", requestId: null });
    });
    renderApp(bridge, "settings");
    await fireEvent.click(await screen.findByTestId("settings-clear-cache"));
    expect((await screen.findByTestId("settings-cache-failed")).textContent).toContain("core busy");
    expect(screen.queryByTestId("settings-cache-cleared")).toBeNull();
    cleanup();

    const silent = createPreloadBridge();
    const client = createUiClient({ preloadBridge: silent.bridge });
    await client.connect();
    expect(await client.clearCache(20)).toEqual({ outcome: "unconfirmed" });
    const confirmed = client.clearCache(1_000);
    silent.emit({ deltaType: "resourceInvalidated", revision: silent.snapshot.revision + 1, resource_kind: "likedSongs" });
    silent.emit({ deltaType: "resourceInvalidated", revision: silent.snapshot.revision + 2, resource_kind: "catalog" });
    expect(await confirmed).toEqual({ outcome: "confirmed" });

    // Over raw envelopes the clear carries a requestId, so core's error reply is correlated.
    const listeners = new Set<(env: Envelope) => void>();
    const raw: RawPreloadUiBridge = {
      onEnvelope: (l) => (listeners.add(l), () => listeners.delete(l)),
      async sendEnvelope(env) {
        if (env.kind === "ui.clearCache") {
          expect(env.requestId).toBeTruthy();
          const reply: Envelope = {
            protocolVersion: PROTOCOL_VERSION, kind: "core.error", coreEpoch: null, requestId: env.requestId, seq: 1,
            payload: { code: "STORAGE_ERROR", retriable: false, retryAfterMs: null, safeMessage: "disk full", requestId: env.requestId },
          };
          queueMicrotask(() => listeners.forEach((l) => l(reply)));
        }
        return { ok: true };
      },
    };
    await expect(wrapPreloadEnvelopeBridge(raw).clearCache!()).rejects.toMatchObject({ error: { safeMessage: "disk full" } });
  });

  it("plays album tracks in the album's audio playlist and falls back when indexes do not align", async () => {
    for (const [playbackIndexes, expected] of [
      [[0, 2, 3], { videoId: "alb-vid-00002", playlistId: "OLAK5uy_test", startIndex: 2 }],
      [[0, 2], { videoId: "alb-vid-00002", playlistId: null, startIndex: null }],
    ] as const) {
      const { bridge } = createPreloadBridge({
        queryCatalog: (req) =>
          req.kind === "albumDetail"
            ? {
                resultType: "albumDetail",
                albumId: req.albumId ?? "",
                title: "Album One",
                tracks: createSyntheticTracks(3, "alb"),
                audioPlaylistId: "OLAK5uy_test",
                playbackIndexes: [...playbackIndexes],
              }
            : new FakeCoreClient("normal").queryCatalog(req),
      });
      renderApp(bridge, "library");
      await fireEvent.click(await screen.findByTestId("playlist-card-mock-pl-favorites"));
      const rows = await screen.findAllByTestId("virtual-track-row");
      await fireEvent.click(within(rows[0]!).getByTestId("track-actions-btn"));
      await fireEvent.click(await screen.findByTestId("action-go-album"));
      await waitFor(() => expect(screen.getByTestId("view-album")).toBeTruthy());
      await waitFor(() => expect(screen.getAllByTestId("virtual-track-row")).toHaveLength(3));
      const albumRows = screen.getAllByTestId("virtual-track-row");
      await fireEvent.click(albumRows[1]!.querySelector("[data-row-index]")!);
      expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "playTarget", args: { target: expected } }));
      cleanup();
    }
  });

  it("shows connection error banner on initial preload failure and recovers via bounded auto-retry or manual reconnect", async () => {
    const fake = new FakeCoreClient("normal");
    const snap = { ...fake.subscribeSnapshot(() => {}).snapshot, mockMode: false };
    let attempts = 0;
    const autoRetryBridge: YtmcpUiBridge = {
      subscribeSnapshot: () => {
        attempts += 1;
        if (attempts === 1) {
          return Promise.reject(new Error("daemon handshake not ready"));
        }
        return { snapshot: structuredClone(snap), unsubscribe: () => {} };
      },
      sendPlayerCommand: vi.fn(),
      queryCatalog: (req) => fake.queryCatalog(req),
      queryLyrics: () => DISABLED_LYRICS,
      updateSettings: vi.fn(async () => snap.settings),
      openPlayerWindow: vi.fn(),
    };

    render(App, {
      props: {
        client: createUiClient({ preloadBridge: autoRetryBridge, connectRetryDelaysMs: [15] }),
      },
    });
    expect(await screen.findByTestId("banner-connection-error")).toBeTruthy();
    expect(screen.getByTestId("banner-connection-error").textContent).toContain("daemon handshake not ready");
    await waitFor(() => expect(screen.queryByTestId("banner-connection-error")).toBeNull());
    expect(screen.queryByTestId("home-state-loading")).toBeNull();
    expect(attempts).toBe(2);
    cleanup();

    const retryDelaysMs = [10, 20];
    let manualAttempts = 0;
    let allowManual = false;
    const manualBridge: YtmcpUiBridge = {
      ...autoRetryBridge,
      subscribeSnapshot: () => {
        manualAttempts += 1;
        if (!allowManual) {
          return Promise.reject(new Error("sidecar crashed during boot"));
        }
        return { snapshot: structuredClone(snap), unsubscribe: () => {} };
      },
    };
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      render(App, {
        props: {
          client: createUiClient({ preloadBridge: manualBridge, connectRetryDelaysMs: retryDelaysMs }),
        },
      });
      // Run far past every scheduled retry: the auto-retry stops after its bounded schedule.
      await vi.advanceTimersByTimeAsync(10_000);
      expect(manualAttempts).toBe(1 + retryDelaysMs.length);
      expect(screen.getByTestId("banner-connection-error").textContent).toContain("sidecar crashed during boot");
    } finally {
      vi.useRealTimers();
    }

    allowManual = true;
    await fireEvent.click(screen.getByTestId("btn-reconnect-core"));
    await waitFor(() => expect(screen.queryByTestId("banner-connection-error")).toBeNull());
    expect(screen.queryByTestId("home-state-loading")).toBeNull();
  });

  it("loads additional playlist pages and recovers from page-load errors inside Add to playlist", async () => {
    let page2Attempts = 0;
    const fakeFallback = new FakeCoreClient("normal");
    const sendMutation = vi.fn(async (): Promise<MutationResult> => ({
      kind: "addToPlaylist",
      outcome: "confirmed",
      error: null,
    }));
    const { bridge } = createPreloadBridge({
      snapshot: writeReady,
      sendMutation,
      queryCatalog: (req) => {
        if (req.kind !== "libraryPlaylists") {
          return fakeFallback.queryCatalog(req);
        }
        if (!req.cursor) {
          return Promise.resolve({
            resultType: "libraryPlaylists",
            ...paged(
              [
                {
                  playlistId: "pl-p1",
                  browseId: null,
                  title: "First Page Mix",
                  subtitle: "Page 1",
                  trackCount: 5,
                  cover: null,
                },
              ],
              "cursor-p2"
            ),
          });
        }
        page2Attempts += 1;
        if (page2Attempts === 1) {
          return Promise.reject(new IpcTimeoutError("ui.catalogQuery"));
        }
        return Promise.resolve({
          resultType: "libraryPlaylists",
          ...paged(
            [
              {
                playlistId: "pl-p2",
                browseId: null,
                title: "Late Page Mix",
                subtitle: "Page 2",
                trackCount: 8,
                cover: null,
              },
            ],
            null
          ),
        });
      },
    });

    renderApp(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-actions"));
    await fireEvent.click(await screen.findByTestId("action-add-to-playlist"));

    expect(await screen.findByTestId("pick-playlist-pl-p1")).toBeTruthy();
    expect(screen.getByTestId("playlist-picker-loaded-hint").textContent).toContain("loaded playlists");
    const filter = screen.getByLabelText(en.track.filterPlaylists) as HTMLInputElement;
    await fireEvent.input(filter, { target: { value: "Late Page" } });
    expect(screen.getByText(en.track.noMatchingPlaylists)).toBeTruthy();
    // Load more stays reachable even when filtering loaded playlists yields zero matches.
    expect(screen.getByTestId("playlist-picker-load-more")).toBeTruthy();

    await fireEvent.input(filter, { target: { value: "" } });
    await fireEvent.click(screen.getByTestId("playlist-picker-load-more"));
    const errBanner = await screen.findByTestId("playlist-picker-more-error");
    expect(errBanner.textContent).toContain("TIMEOUT");

    await fireEvent.click(screen.getByTestId("playlist-picker-load-more"));
    await fireEvent.click(await screen.findByTestId("pick-playlist-pl-p2"));
    await fireEvent.click(await screen.findByTestId("confirm-add-to-playlist"));
    await waitFor(() => expect(screen.getByTestId("banner-mutation").getAttribute("data-tone")).toBe("success"));
    expect(sendMutation).toHaveBeenLastCalledWith(
      expect.objectContaining({ kind: "addToPlaylist", playlistId: "pl-p2", confirmedByUser: true }),
      "mock-account-epoch-1"
    );
  });

  it("focuses the search input after async view transitions, without transitions, and under reduced motion", async () => {
    const origDescriptor = Object.getOwnPropertyDescriptor(document, "startViewTransition");
    const setStartViewTransition = (fn: ((cb: () => void | Promise<void>) => unknown) | undefined) => {
      if (fn === undefined) {
        Object.defineProperty(document, "startViewTransition", {
          value: undefined,
          configurable: true,
          writable: true,
        });
        return;
      }
      Object.defineProperty(document, "startViewTransition", {
        value: fn,
        configurable: true,
        writable: true,
      });
    };
    try {
      setStartViewTransition(
        vi.fn((cb: () => void | Promise<void>) => {
          const updateCallbackDone = new Promise<void>((resolve) => {
            setTimeout(() => {
              void Promise.resolve(cb()).then(resolve);
            }, 20);
          });
          return { updateCallbackDone };
        })
      );
      renderApp(createPreloadBridge().bridge, "home");
      await screen.findByTestId("view-home");
      await fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
      await waitFor(() => expect(document.activeElement?.id).toBe("ytm-search-input"));
      cleanup();

      setStartViewTransition(undefined);
      renderApp(createPreloadBridge().bridge, "home");
      await screen.findByTestId("view-home");
      await fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
      await waitFor(() => expect(document.activeElement?.id).toBe("ytm-search-input"));
      cleanup();

      const skippedSpy = vi.fn(() => {
        throw new Error("should not call startViewTransition under reduceMotion");
      });
      setStartViewTransition(skippedSpy);
      renderApp(
        createPreloadBridge({
          snapshot: (s) => ({ ...s, settings: { ...s.settings, reduceMotion: true } }),
        }).bridge,
        "home"
      );
      await screen.findByTestId("view-home");
      await fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
      await waitFor(() => expect(document.activeElement?.id).toBe("ytm-search-input"));
      expect(skippedSpy).not.toHaveBeenCalled();
    } finally {
      if (origDescriptor) {
        Object.defineProperty(document, "startViewTransition", origDescriptor);
      } else {
        Reflect.deleteProperty(document, "startViewTransition");
      }
    }
  });

  it("preserves a visible tabindex=0 row after scrolling 1,000 tracks without stealing focus", async () => {
    const tracks = createSyntheticTracks(1000, "kb");
    const entryIds = tracks.map((_, idx) => `qe-kb-${idx}`);
    const onPlayTrack = vi.fn();
    render(VirtualizedTrackList, {
      props: {
        tracks,
        entryIds,
        mockMode: true,
        heightPx: 480,
        onPlayTrack,
      },
    });

    const list = screen.getByTestId("virtual-track-list");
    Object.defineProperty(list, "clientHeight", { value: 480, configurable: true });
    list.scrollTop = 5800;
    await fireEvent.scroll(list);

    let visibleIdx = -1;
    await waitFor(() => {
      const tabZeroButtons = list.querySelectorAll<HTMLElement>('[data-row-index][tabindex="0"]');
      expect(tabZeroButtons).toHaveLength(1);
      visibleIdx = Number(tabZeroButtons[0]!.getAttribute("data-row-index"));
      expect(visibleIdx).toBeGreaterThan(50);
    });
    expect(list.contains(document.activeElement)).toBe(false);

    const entryButton = list.querySelector<HTMLElement>(`[data-row-index="${visibleIdx}"][tabindex="0"]`)!;
    entryButton.focus();
    await tick();
    expect(document.activeElement).toBe(entryButton);

    await fireEvent.keyDown(list, { key: "ArrowDown" });
    const nextIdx = visibleIdx + 1;
    await waitFor(() => {
      expect(document.activeElement?.getAttribute("data-row-index")).toBe(String(nextIdx));
    });
    await fireEvent.click(document.activeElement!);
    expect(onPlayTrack).toHaveBeenCalledWith(tracks[nextIdx], nextIdx, `qe-kb-${nextIdx}`);
  });

  it("configures Moment DJ sharing in Settings and updates the live principal preview", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => writeReady(s),
    });
    renderApp(bridge, "settings");

    const preview = await screen.findByTestId("moment-preview-json");
    expect(JSON.parse(preview.textContent ?? "{}").environment.status).toBe("notShared");

    const shareTime = screen.getByTestId("moment-share-time") as HTMLInputElement;
    await fireEvent.click(shareTime);
    await waitFor(() => expect(shareTime.checked).toBe(true));
    const principalToggle = screen.getByTestId("moment-principal-mock-dj") as HTMLInputElement;
    await fireEvent.click(principalToggle);

    await waitFor(() => {
      const parsed = JSON.parse(screen.getByTestId("moment-preview-json").textContent ?? "{}");
      expect(parsed.environment.status).toBe("shared");
      expect(parsed.environment.time.timeZone).toBeTruthy();
    });
  });

  it("rolls back Now Playing seek preview on a failed command and throttles rapid volume drags", async () => {
    const { bridge, snapshot } = createPreloadBridge();
    bridge.sendPlayerCommand.mockImplementation(async (cmd): Promise<CommandReceipt> => ({
      commandId: cmd.commandId,
      op: cmd.op,
      status: cmd.op === "seek" ? "failed" : "observed",
      acceptedAtMs: 0,
      observedAtMs: cmd.op === "seek" ? null : 1,
      deduplicated: false,
      error: cmd.op === "seek"
        ? { code: "TIMEOUT", retriable: true, retryAfterMs: null, safeMessage: "seek timed out", requestId: null }
        : null,
    }));
    renderApp(bridge, "now-playing");

    const npSlider = (await screen.findByTestId("np-seek-slider")) as HTMLInputElement;
    const beforePos = screen.getByTestId("np-pos-text").textContent;
    await fireEvent.input(npSlider, { target: { value: "90000" } });
    expect(screen.getByTestId("np-pos-text").textContent).toBe("1:30");
    await fireEvent.change(npSlider, { target: { value: "90000" } });
    await waitFor(() => {
      expect(screen.getByTestId("np-pos-text").textContent).toBe(beforePos);
    });

    const volSlider = screen.getByTestId("player-volume-slider") as HTMLInputElement;
    bridge.sendPlayerCommand.mockClear();
    await fireEvent.pointerDown(volSlider);
    await fireEvent.input(volSlider, { target: { value: "40" } });
    await fireEvent.input(volSlider, { target: { value: "45" } });
    await fireEvent.input(volSlider, { target: { value: "52" } });
    // Leading edge sends first drag event immediately; intermediate drags wait for throttle or release.
    expect(bridge.sendPlayerCommand).toHaveBeenCalledTimes(1);
    await fireEvent.change(volSlider, { target: { value: "52" } });
    await waitFor(() => {
      expect(bridge.sendPlayerCommand).toHaveBeenCalledTimes(2);
      expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
        expect.objectContaining({ op: "setVolume", args: { volume: 0.52 } })
      );
    });
    void snapshot;
  });
});
