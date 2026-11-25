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
  type RecentSearchesRequest,
  type RecentSearchesSnapshot,
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
import { TOAST_DWELL_MS } from "../../lib/feedback/toasts.svelte.js";
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
  recentSearches?: (req: RecentSearchesRequest, accountEpoch: string | null) => RecentSearchesSnapshot | Promise<RecentSearchesSnapshot>;
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
    recentSearches: options.recentSearches,
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
    await screen.findByTestId("view-home");
    // The scenario select is in Settings (Advanced), never in the page flow.
    expect(screen.queryByTestId("mock-scenario-select")).toBeNull();
    const pick = async (value: string) => {
      await fireEvent.click(screen.getByTestId("nav-settings"));
      await fireEvent.change(await screen.findByTestId("mock-scenario-select"), { target: { value } });
      await fireEvent.click(screen.getByTestId("nav-home"));
    };

    await pick("loading");
    await waitFor(() => expect(screen.getByTestId("home-state-loading")).toBeTruthy());

    await pick("empty");
    await waitFor(() => expect(screen.getByTestId("home-state-empty")).toBeTruthy());

    await pick("offlineCached");
    await waitFor(() => {
      expect(screen.getByTestId("toast-offline")).toBeTruthy();
      // The offline toast says it; the page adds no second note.
      expect(screen.queryByTestId("toast-refresh-failed")).toBeNull();
    });

    await pick("authExpired");
    await waitFor(() => {
      expect(screen.getByTestId("toast-auth-expired")).toBeTruthy();
      expect(screen.getByTestId("home-state-auth-expired")).toBeTruthy();
    });

    await pick("unsupported");
    await waitFor(() => {
      expect(screen.getByTestId("toast-unsupported")).toBeTruthy();
      expect((screen.getByTestId("btn-play-pause") as HTMLButtonElement).disabled).toBe(true);
    });

    await pick("bridgeCrashed");
    await waitFor(() => {
      expect(screen.getByTestId("toast-bridge-disconnected")).toBeTruthy();
      expect((screen.getByTestId("btn-play-pause") as HTMLButtonElement).disabled).toBe(true);
    });
    expect(fetchSpy).not.toHaveBeenCalled();

    cleanup();
    renderApp(createPreloadBridge().bridge);
    await waitFor(() => expect(screen.getByTestId("view-home")).toBeTruthy());
    expect(screen.getByTestId("toast-region").children).toHaveLength(0);
    await fireEvent.click(screen.getByTestId("nav-settings"));
    await screen.findByTestId("settings-advanced");
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
    // The code stays out of the visible text; the tooltip carries it.
    expect(errorBox.textContent).not.toContain("RATE_LIMITED");
    expect(errorBox.getAttribute("title")).toContain("RATE_LIMITED");
    await fireEvent.click(within(errorBox).getByRole("button"));
    await waitFor(() => expect(failing.catalogCalls.filter((r) => r.kind === "libraryPlaylists")).toHaveLength(2));
    cleanup();

    render(App, { props: { client: createUiClient({ mockScenario: "offlineCached" }), initialView: "library" } });
    await waitFor(() => {
      expect(screen.getByTestId("toast-offline")).toBeTruthy();
      expect(screen.getByTestId("playlist-card-mock-pl-favorites")).toBeTruthy();
    });
    expect(screen.queryByTestId("toast-refresh-failed")).toBeNull();
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
    // The kept content is reported by a toast, not a note in the page.
    const note = await screen.findByTestId("toast-refresh-failed");
    expect(note.querySelector(".toast-text")!.getAttribute("title")).toContain("TIMEOUT");
    expect(note.textContent).toContain(en.states.refreshFailedNotice);
    expect(note.textContent).not.toContain("TIMEOUT");
    expect(screen.getByTestId("playlist-card-mock-pl-favorites")).toBeTruthy();

    failWith = new Error("IPC rejected ui.catalogQuery");
    invalidate(snapshot.revision + 2);
    const errorBox = await screen.findByTestId("library-state-error");
    expect(errorBox.getAttribute("title")).toContain("BRIDGE_INCOMPATIBLE");
    expect(errorBox.textContent).not.toContain("BRIDGE_INCOMPATIBLE");
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
      expect(screen.getAllByTestId("virtual-track-row").length).toBeGreaterThanOrEqual(2);
    });

    // The grouped view's short Songs block never pages; the full list behind "All songs" asks for
    // the next page by itself as its end comes near, with no Load more button.
    expect(catalogCalls.filter((r) => r.kind === "searchSongs" && r.cursor === "cur-1")).toHaveLength(0);
    await fireEvent.click(screen.getByTestId("search-all-songs"));
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

  /** A core list per epoch, answered when the test says so. */
  function heldRecentSearches() {
    const calls: Array<{ req: RecentSearchesRequest; epoch: string | null; resolve: (items: string[]) => void }> = [];
    const recentSearches = vi.fn(
      (req: RecentSearchesRequest, epoch: string | null) =>
        new Promise<RecentSearchesSnapshot>((resolve) => {
          calls.push({
            req,
            epoch,
            resolve: (items) => resolve({ items: items.map((query) => ({ query, recordedAtMs: 1 })) }),
          });
        })
    );
    return { calls, recentSearches };
  }

  const chipTexts = () =>
    screen.queryAllByTestId("search-recent-chip").map((chip) => chip.textContent?.trim());

  it("asks the core for the signed-in account's recent searches and records and clears through it", async () => {
    const fake = new FakeCoreClient("normal");
    const recentSearches = vi.fn((req: RecentSearchesRequest, epoch: string | null) => fake.recentSearches(req, epoch));
    fake.recentSearches({ action: "record", query: "from an earlier run" }, "mock-account-epoch-1");
    const { bridge } = createPreloadBridge({ queryCatalog: searchCatalog, recentSearches });
    renderApp(bridge, "search");
    await waitFor(() => expect(chipTexts()).toEqual(["from an earlier run"]));
    expect(recentSearches).toHaveBeenCalledWith({ action: "list" }, "mock-account-epoch-1");
    expect(localStorage.length).toBe(0);

    await submitSearch("  Kept query  ");
    expect(recentSearches).toHaveBeenLastCalledWith({ action: "record", query: "Kept query" }, "mock-account-epoch-1");
    await fireEvent.click(screen.getByTestId("search-clear-btn"));
    await waitFor(() => expect(chipTexts()).toEqual(["Kept query", "from an earlier run"]));

    await fireEvent.click(screen.getByTestId("search-recent-clear"));
    expect(recentSearches).toHaveBeenLastCalledWith({ action: "clear" }, "mock-account-epoch-1");
    await waitFor(() => expect(screen.queryByTestId("search-recent")).toBeNull());
  });

  it("never shows the old account's list after an account change, even when its reply comes late", async () => {
    const held = heldRecentSearches();
    const { bridge, snapshot, emit } = createPreloadBridge({ queryCatalog: searchCatalog, recentSearches: held.recentSearches });
    // Asked for at sign-in whatever page is open.
    renderApp(bridge, "home");
    await waitFor(() => expect(held.calls).toHaveLength(1));
    expect(held.calls[0]!.epoch).toBe("mock-account-epoch-1");

    // The core announces the new account, then invalidates account state: the new list is
    // already on its way when the invalidation lands.
    emit({
      deltaType: "authChanged",
      revision: snapshot.revision + 1,
      auth_state: { ...snapshot.authState, accountEpoch: "acct-2", accountScope: "scope-acct-2" },
    });
    await waitFor(() => expect(held.calls.some((c) => c.epoch === "acct-2")).toBe(true));
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 2, resource_kind: "accountState" });
    await new Promise((r) => setTimeout(r, 0));

    held.calls[0]!.resolve(["first account private"]);
    await new Promise((r) => setTimeout(r, 0));
    await fireEvent.click(screen.getByTestId("nav-search"));
    await screen.findByTestId("view-search");
    expect(screen.queryByTestId("search-recent")).toBeNull();

    await fireEvent.click(screen.getByTestId("nav-home"));
    await screen.findByTestId("view-home");
    for (const call of held.calls.filter((c) => c.epoch === "acct-2")) call.resolve(["second account"]);
    await new Promise((r) => setTimeout(r, 0));
    await fireEvent.click(screen.getByTestId("nav-search"));
    await waitFor(() => expect(chipTexts()).toEqual(["second account"]));
    expect(screen.queryByText("first account private")).toBeNull();
  });

  it("empties the list on sign-out without asking the core to clear it", async () => {
    const fake = new FakeCoreClient("normal");
    fake.recentSearches({ action: "record", query: "mine" }, "mock-account-epoch-1");
    const recentSearches = vi.fn((req: RecentSearchesRequest, epoch: string | null) => fake.recentSearches(req, epoch));
    const { bridge, snapshot, emit } = createPreloadBridge({ queryCatalog: searchCatalog, recentSearches });
    renderApp(bridge, "search");
    await waitFor(() => expect(chipTexts()).toEqual(["mine"]));
    emit({
      deltaType: "authChanged",
      revision: snapshot.revision + 1,
      auth_state: { ...snapshot.authState, state: "signedOut", accountEpoch: null, accountScope: null },
    });
    await waitFor(() => expect(screen.queryByTestId("search-recent")).toBeNull());
    expect(recentSearches.mock.calls.some(([req]) => req.action === "clear")).toBe(false);
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
    await screen.findByRole("heading", { name: title });
    await fireEvent.click(screen.getByTestId("nav-now-playing"));
    await screen.findByTestId("view-now-playing");

    emit({
      deltaType: "resourceInvalidated", revision: snapshot.revision + 1,
      resource_kind: "playlistTracks", resource_id: "another-playlist",
    });
    await fireEvent.click(screen.getByTestId("nav-library"));
    await fireEvent.click(await playlist());
    await screen.findByRole("heading", { name: title });
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
    await screen.findByRole("heading", { name: title });
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

  it("keeps an unknown write in Diagnostics after its toast has gone, marked until the listener has checked it", async () => {
    const sendMutation = vi.fn(
      async (): Promise<MutationResult> => ({
        kind: "setLike",
        outcome: "indeterminate",
        error: { code: "TIMEOUT", retriable: false, retryAfterMs: null, safeMessage: "no answer from the player", requestId: null },
      })
    );
    const { bridge, emit, snapshot } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-like"));
    await screen.findByTestId("toast-write-unknown");
    // A later toast replaces it before anyone has read it.
    emit({
      deltaType: "commandUpdated",
      revision: snapshot.revision + 1,
      receipt: { commandId: "c-x", op: "next", status: "failed", acceptedAtMs: 0, observedAtMs: null, deduplicated: false,
        error: { code: "BUSY", retriable: true, retryAfterMs: null, safeMessage: "player busy", requestId: null } },
    });
    await waitFor(() => expect(screen.queryByTestId("toast-write-unknown")).toBeNull());

    // The failure's own toast lands on its entry; the unknown write is listed below it.
    await fireEvent.click(await screen.findByTestId("toast-view-diagnostics"));
    const problems = await screen.findByTestId("diagnostics-problems");
    const entries = within(problems).getAllByTestId("problem-entry");
    expect(entries).toHaveLength(2);
    await waitFor(() => expect(document.activeElement).toBe(entries[0]));
    expect(entries[0]!.textContent).toContain("Couldn’t skip to the next song");
    expect(within(entries[0]!).getByTestId("problem-technical").textContent).toBe("BUSY: player busy");
    const unknown = entries[1]!;
    expect(unknown.textContent).toContain(en.toast.writeUnknown);
    expect(unknown.textContent).toContain(en.dialog.checkWriteAdvice);
    expect(within(unknown).getByTestId("problem-technical").textContent).toBe("TIMEOUT: no answer from the player");
    expect(within(unknown).getByTestId("problem-not-checked")).toBeTruthy();
    await fireEvent.keyDown(screen.getByTestId("accessible-dialog"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("accessible-dialog")).toBeNull());

    // Settings > Advanced shows how many there are; the entry stays until it is marked checked.
    await fireEvent.click(screen.getByTestId("nav-settings"));
    expect((await screen.findByTestId("settings-problem-count")).textContent).toContain("2");
    await fireEvent.click(screen.getByTestId("settings-open-diagnostics"));
    const again = within(await screen.findByTestId("diagnostics-problems")).getAllByTestId("problem-entry")[1]!;
    await fireEvent.click(within(again).getByTestId("problem-mark-checked"));
    expect(within(screen.getByTestId("diagnostics-problems")).getByTestId("problem-checked")).toBeTruthy();
    expect(screen.queryByTestId("problem-not-checked")).toBeNull();
  });

  it("keeps a failure toast on screen while the window is hidden and counts down once it is back", async () => {
    let hidden = false;
    vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
    const sendMutation = vi.fn(async (): Promise<MutationResult> => ({
      kind: "setLike",
      outcome: "rejected",
      error: { code: "RATE_LIMITED", retriable: false, retryAfterMs: null, safeMessage: "slow down", requestId: null },
    }));
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    const like = await screen.findByTestId("np-like");
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    try {
      hidden = true;
      document.dispatchEvent(new Event("visibilitychange"));
      await fireEvent.click(like);
      await vi.advanceTimersByTimeAsync(0);
      expect(screen.getByTestId("toast-write-failed")).toBeTruthy();
      await vi.advanceTimersByTimeAsync(60_000);
      expect(screen.getByTestId("toast-write-failed")).toBeTruthy();
      hidden = false;
      document.dispatchEvent(new Event("visibilitychange"));
      await vi.advanceTimersByTimeAsync(TOAST_DWELL_MS + 1_000);
      expect(screen.queryByTestId("toast-write-failed")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("starts the problem log empty for another account", async () => {
    const sendMutation = vi.fn(async (): Promise<MutationResult> => ({
      kind: "setLike",
      outcome: "rejected",
      error: { code: "RATE_LIMITED", retriable: false, retryAfterMs: null, safeMessage: "refused", requestId: null },
    }));
    const { bridge, emit, snapshot } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-like"));
    await screen.findByTestId("toast-write-failed");
    await fireEvent.click(screen.getByTestId("nav-settings"));
    expect((await screen.findByTestId("settings-problem-count")).textContent).toContain("1");
    emit({
      deltaType: "authChanged",
      revision: snapshot.revision + 1,
      auth_state: { ...snapshot.authState, state: "authenticated", accountEpoch: "acct-2", accountScope: "scope-acct-2", playerCatalogMatched: true },
    });
    await waitFor(() => expect(screen.queryByTestId("settings-problem-count")).toBeNull());
    await fireEvent.click(screen.getByTestId("settings-open-diagnostics"));
    expect(await screen.findByTestId("diagnostics-problems-empty")).toBeTruthy();
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
    expect(likeBtn.getAttribute("aria-label")).toBe("Remove like"); // synthetic track #1 is liked
    await fireEvent.click(likeBtn);
    await waitFor(() => {
      const toast = screen.getByTestId("toast-write-unknown");
      expect(toast.getAttribute("data-tone")).toBe("warning");
      expect(toast.textContent).toContain("Couldn’t confirm");
    });
    expect(sendMutation).toHaveBeenCalledTimes(1);
    expect(sendMutation).toHaveBeenCalledWith(
      { kind: "setLike", videoId: "mock-vid-00001", liked: false, playlistId: null, confirmedByUser: true },
      "mock-account-epoch-1"
    );
    expect(likeBtn.getAttribute("aria-label")).toBe("Remove like"); // unknown outcome is not applied

    await fireEvent.click(screen.getByTestId("np-actions"));
    await fireEvent.click(await screen.findByTestId("action-add-to-playlist"));
    // The pick is the explicit request: it is sent at once, and the playlist is not in view, so
    // its result is a toast (no Undo here: this reply carries no undo token).
    await fireEvent.click(await screen.findByTestId("pick-playlist-mock-pl-late-night"));
    await waitFor(() => expect(screen.getByTestId("toast-write-done").textContent).toContain("Added to"));
    expect(screen.queryByTestId("toast-undo")).toBeNull();
    expect(sendMutation).toHaveBeenCalledTimes(2);
    expect(sendMutation).toHaveBeenLastCalledWith(
      expect.objectContaining({ kind: "addToPlaylist", playlistId: "mock-pl-late-night", confirmedByUser: true }),
      "mock-account-epoch-1"
    );
  });

  it("opens the playlist chooser straight from Now Playing's Add to playlist, only while adding can work", async () => {
    const sendMutation = vi.fn(async (): Promise<MutationResult> => ({ kind: "addToPlaylist", outcome: "confirmed", error: null }));
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-add-to-playlist"));
    expect(screen.queryByTestId("action-play")).toBeNull();
    await fireEvent.click(await screen.findByTestId("pick-playlist-mock-pl-late-night"));
    await waitFor(() =>
      expect(sendMutation).toHaveBeenLastCalledWith(
        expect.objectContaining({ kind: "addToPlaylist", videoId: "mock-vid-00001", playlistId: "mock-pl-late-night" }),
        "mock-account-epoch-1"
      )
    );
    cleanup();

    const readOnly = createPreloadBridge({
      snapshot: (s) => ({ ...writeReady(s), capabilities: { ...writeReady(s).capabilities, playlistAddMutation: "unverified" } }),
      sendMutation,
    });
    renderApp(readOnly.bridge, "now-playing");
    await screen.findByTestId("now-playing-title");
    expect(screen.queryByTestId("np-add-to-playlist")).toBeNull();
  });

  it("leaves out like on Now Playing when the write cannot work, and explains other writes in the actions dialog", async () => {
    const unverified = createPreloadBridge({
      snapshot: (s) => ({ ...s, capabilities: { ...s.capabilities, likeMutation: "unverified" } }),
      sendMutation: vi.fn(),
    });
    renderApp(unverified.bridge, "now-playing");
    await screen.findByTestId("now-playing-title");
    const page = screen.getByTestId("view-now-playing");
    expect(screen.queryByTestId("np-like")).toBeNull();
    expect(screen.queryByTestId("player-like")).toBeNull();
    expect(page.textContent).not.toContain("This change is not verified yet");
    cleanup();

    const unmatched = createPreloadBridge({
      snapshot: (s) => ({
        ...writeReady(s),
        capabilities: { ...writeReady(s).capabilities, queueEnqueue: "unsupported" },
        authState: { ...s.authState, playerCatalogMatched: false },
      }),
    });
    renderApp(unmatched.bridge, "now-playing");
    await screen.findByTestId("now-playing-title");
    expect(screen.getByTestId("view-now-playing").textContent).not.toContain("not confirmed to match");
    expect(screen.queryByTestId("np-like")).toBeNull();

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
    expect(repeat.title).toBe(en.player.repeat.off);
    await fireEvent.click(repeat);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "setRepeat", args: { repeat: "all" } }));

    await fireEvent.click(rows[2]!.querySelector("[data-row-index]")!);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: "selectQueueEntry", args: { queueEntryId: "qe-mock-3", queueRevision: 1 } })
    );
  });

  it("starts an Up next row on Now Playing with the queue's own command, and leads to the queue", async () => {
    const { bridge } = createPreloadBridge({ snapshot: writeReady });
    renderApp(bridge, "now-playing");
    const upNext = await screen.findByTestId("np-up-next");
    const start = (await within(upNext).findAllByTestId("np-up-next-start"))[0]!;
    await fireEvent.click(start);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: "selectQueueEntry", args: { queueEntryId: "qe-mock-2", queueRevision: 1 } })
    );
    await fireEvent.click(within(upNext).getByTestId("np-up-next-open-queue"));
    await screen.findByTestId("view-queue");
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
      props: { snapshot: snap, pendingOverlay: overlay, interpolatedPositionMs: 12_000, onCommand, onOpenNowPlaying: () => {}, onToggleLyrics: () => {} },
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
      props: { snapshot: snap, pendingOverlay: pendingVolume, interpolatedPositionMs: 12_000, onCommand, onOpenNowPlaying: () => {}, onToggleLyrics: () => {} },
    });
    const bar = screen.getByTestId("player-bar");
    const before = bar.textContent;
    const slider = screen.getByTestId("player-volume-slider") as HTMLInputElement;

    await fireEvent.pointerDown(slider);
    await fireEvent.input(slider, { target: { value: "30" } });
    expect(onCommand).toHaveBeenLastCalledWith("setVolume", { volume: 0.3 });
    // The slider announces the level and the controls use their names as tooltips.
    expect(bar.textContent).toBe(before);
    expect(slider.value).toBe("30");
    expect(slider.getAttribute("aria-valuetext")).toBe("30%");
    expect(slider.title).toBe(en.player.volumeLabel);
    expect(screen.getByTestId("btn-mute").title).toBe(en.player.mute);
    expect(screen.queryByTestId("player-waiting-dot")).toBeNull();
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
      props: { tracks: createSyntheticTracks(10_000, "test10k"), mockMode: true, onPlayTrack: () => {} },
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

    // The position clock is one timeout at a time (it used to be a 200 ms interval), so the
    // timeouts are faked; highlights now switch at their line time, not up to 200 ms later.
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
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
      // Sampled every 20 ms: a line shows within one sample of its time.
      expect(delays.at(-1)).toBeLessThanOrEqual(40);

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
    // Playing or paused is the play button's icon; its label names the action it would take.
    const status = () => screen.getByTestId("btn-play-pause").getAttribute("aria-label");
    await waitFor(() => expect(screen.getByTestId("search-input")).toBeTruthy());
    expect(status()).toBe("Play");

    const input = screen.getByTestId("search-input") as HTMLInputElement;
    input.focus();
    await fireEvent.keyDown(input, { key: " ", code: "Space" });
    expect(status()).toBe("Play");

    input.blur();
    await fireEvent(window, new CompositionEvent("compositionstart", { data: "ni" }));
    await fireEvent.keyDown(document.body, { key: " ", code: "Space", isComposing: true });
    expect(status()).toBe("Play");

    await fireEvent(window, new CompositionEvent("compositionend", { data: "你" }) /* pinyin "ni" committed */);
    await fireEvent.keyDown(document.body, { key: " ", code: "Space" });
    await waitFor(() => expect(status()).toBe("Pause"));
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
    expect(screen.getByTestId("btn-play-pause").getAttribute("aria-label")).toBe("播放");
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
    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }), initialView: "settings" } });
    const trigger = (await screen.findByTestId("settings-open-diagnostics")) as HTMLButtonElement;
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
    const saveFailed = await screen.findByTestId("toast-settings-failed");
    expect(saveFailed.textContent).toContain("Couldn’t save the setting");
    expect(saveFailed.textContent).not.toContain("settings disk read-only");
    expect(saveFailed.querySelector(".toast-text")!.getAttribute("title")).toContain("settings disk read-only");
    await waitFor(() => expect(autoplay.checked).toBe(base.autoplayOnLaunch));
  });

  it("wires official player open, account match status, cache clear feedback, search clear, now-playing seek, failure toast and localized diagnostics", async () => {
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

    // A failed command is a toast in plain words; its code is in Diagnostics, one click away.
    const failure = screen.getByTestId("toast-command-failed");
    expect(failure.textContent).toContain("Couldn’t start playing");
    expect(failure.textContent).not.toContain("player timed out");
    expect(failure.textContent).not.toContain("TIMEOUT");
    await fireEvent.click(screen.getByTestId("toast-view-diagnostics"));
    expect((await screen.findByTestId("diagnostics-content")).textContent).toContain("TIMEOUT: player timed out");
    // The failure reported with the first snapshot is in Recent problems, and the toast lands on it.
    const landed = within(screen.getByTestId("diagnostics-problems")).getByTestId("problem-entry");
    await waitFor(() => expect(document.activeElement).toBe(landed));
    expect(within(landed).getByTestId("problem-technical").textContent).toBe("TIMEOUT: player timed out");
    await fireEvent.keyDown(screen.getByTestId("accessible-dialog"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("accessible-dialog")).toBeNull());
    expect(screen.queryByTestId("toast-command-failed")).toBeNull();

    await fireEvent.click(screen.getByTestId("settings-open-diagnostics"));
    const diag = await screen.findByTestId("diagnostics-content");
    expect(diag.textContent).toContain("GPU compositing");
    expect(diag.textContent).toContain("MPRIS media controls");
    expect(diag.textContent).toContain("Play / pause");
    expect(diag.textContent).toContain("Supported");
    await fireEvent.keyDown(screen.getByTestId("accessible-dialog"), { key: "Escape" });

    await fireEvent.click(screen.getByTestId("nav-now-playing"));
    await screen.findByTestId("view-now-playing");
    // Now Playing has no seek bar of its own; the player bar's seeks from every page.
    expect(screen.queryAllByRole("slider", { name: "Playback position" })).toHaveLength(1);
    await fireEvent.change(screen.getByTestId("player-seek-slider"), { target: { value: "42000" } });
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

  it("puts glass only on the sidebar, the player bar and overlays, and keeps list text off the bleed layers", async () => {
    render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }) } });
    await screen.findByTestId("home-playlist-mock-pl-favorites");
    const glass = () => Array.from(document.querySelectorAll(".lg")).map((el) => el.getAttribute("data-testid"));
    expect(glass()).toEqual(["app-sidebar", "player-bar"]);
    await fireEvent.click(screen.getAllByTestId("track-actions-btn")[0]!);
    await screen.findByTestId("track-actions-menu");
    expect(glass()).toEqual(["app-sidebar", "player-bar", "track-actions-menu"]);

    // Only the cover shelf leaves the page column; track rows never do. The ambient light is a
    // window layer outside the scroll container, so pages paint no wash of their own.
    expect(screen.getByTestId("cover-shelf").classList.contains("bleed")).toBe(true);
    expect(screen.getByTestId("ambient-light").closest('[data-testid="main-scroll-region"]')).toBeNull();
    for (const row of screen.getAllByTestId("virtual-track-row")) expect(row.closest(".bleed")).toBeNull();
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

  it("sends recent-search requests with the account epoch and turns core.error into CoreRequestError", async () => {
    const fake = new FakeCoreClient("normal");
    const sent: Envelope[] = [];
    const listeners = new Set<(env: Envelope) => void>();
    const rawPreload: RawPreloadUiBridge = {
      onEnvelope(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      async sendEnvelope(env) {
        sent.push(env);
        let kind: Envelope["kind"] = "core.recentSearches";
        let payload: unknown;
        try {
          payload = fake.recentSearches(env.payload as RecentSearchesRequest, env.accountEpoch ?? null);
        } catch (err) {
          kind = "core.error";
          payload = (err as { error: unknown }).error;
        }
        const out: Envelope = { protocolVersion: PROTOCOL_VERSION, kind, coreEpoch: null, requestId: env.requestId, seq: 1, payload };
        queueMicrotask(() => { for (const l of listeners) l(out); });
        return { ok: true };
      },
    };
    const bridge = wrapPreloadEnvelopeBridge(rawPreload);
    const reply = await bridge.recentSearches!({ action: "record", query: "q" }, "mock-account-epoch-1");
    expect(reply.items.map((i) => i.query)).toEqual(["q"]);
    expect(sent[0]).toMatchObject({ kind: "ui.recentSearches", accountEpoch: "mock-account-epoch-1" });
    const refused = await Promise.resolve(bridge.recentSearches!({ action: "list" }, "other-epoch")).catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(CoreRequestError);
    expect((refused as CoreRequestError).error.code).toBe("ACCOUNT_MISMATCH");

    // Mock mode answers from the fake core, with the same error type.
    const mock = createUiClient({ fakeClient: new FakeCoreClient("normal") });
    await mock.connect();
    const mockRefused = await mock.recentSearches({ action: "list" }, "other-epoch").catch((e: unknown) => e);
    expect(mockRefused).toBeInstanceOf(CoreRequestError);
    expect((mockRefused as CoreRequestError).error.code).toBe("ACCOUNT_MISMATCH");
    expect((await mock.recentSearches({ action: "list" }, "mock-account-epoch-1")).items).toEqual([]);
    mock.disconnect();
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

  /** All five rows in queue order; the Played section starts collapsed, so it is opened first. */
  async function queueRows() {
    await screen.findAllByTestId("virtual-track-row");
    const played = screen.queryByTestId("queue-played-toggle");
    if (played?.getAttribute("aria-expanded") === "false") await fireEvent.click(played);
    const rows = await screen.findAllByTestId("virtual-track-row");
    expect(rows).toHaveLength(5);
    return rows;
  }
  const button = (row: HTMLElement, id: string) => within(row).getByTestId(id) as HTMLButtonElement;

  it("draws no Clear upcoming while the page refuses clearing, single edits still work", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => {
        const queue = editableQueue(s);
        return { ...queue, capabilities: { ...queue.capabilities, queueClear: "unsupported" } };
      },
    });
    renderApp(bridge, "queue");
    const rows = await queueRows();
    expect(screen.queryByTestId("queue-clear-upcoming")).toBeNull();
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
    expect(screen.getByTestId("toast-command-failed").textContent).toContain("Couldn’t remove that song");
    expect(screen.getByTestId("toast-command-failed").textContent).not.toContain("queue changed");
  });

  it("draws no edits on partial or truncated queues, disables them while unanchored, and no autoplay switch while unknown", async () => {
    // Clearing needs only the anchored current occurrence, not a complete queue (core clears
    // the loaded upcoming batch), so it stays available on the first three.
    for (const [snap, clearEnabled, editsDrawn] of [
      [(s: AppSnapshot) => editableQueue(s, { completeness: "partial" }), true, false],
      [(s: AppSnapshot) => editableQueue(s, { hasMore: null }), true, false],
      [(s: AppSnapshot) => editableQueue(s, { loadedCount: 120 }), true, false],
      [(s: AppSnapshot) => ({ ...editableQueue(s), player: { ...editableQueue(s).player, trackInstanceId: null } }), false, true],
    ] as const) {
      const { bridge } = createPreloadBridge({ snapshot: snap });
      renderApp(bridge, "queue");
      const rows = await queueRows();
      for (const id of ["queue-move-up", "queue-move-down", "queue-remove"]) {
        if (editsDrawn) expect(button(rows[3]!, id).disabled).toBe(true);
        else expect(within(rows[3]!).queryByTestId(id)).toBeNull();
      }
      expect((screen.getByTestId("queue-clear-upcoming") as HTMLButtonElement).disabled).toBe(!clearEnabled);
      // No sentence about why: nothing is added to the page above the list.
      expect(screen.queryByTestId("queue-note")).toBeNull();
      expect(screen.getByTestId("view-queue").textContent).not.toMatch(/fully loaded|finite|not supported|current track/i);
      cleanup();
    }
    const { bridge } = createPreloadBridge({
      snapshot: (s) => ({ ...editableQueue(s), player: { ...editableQueue(s).player, autoplay: null } }),
    });
    renderApp(bridge, "queue");
    await queueRows();
    expect(screen.queryByTestId("queue-autoplay-toggle")).toBeNull();
    expect(screen.getByTestId("view-queue").textContent).not.toContain("Unknown");
  });

  it("offers Clear upcoming on an autoplay tail without single edits, gated only on the anchored current track", async () => {
    // YouTube Music's autoplay tail: a partial queue with more to load; remove/reorder refused.
    const tail = (s: AppSnapshot): AppSnapshot => {
      const queue = editableQueue(s, { completeness: "partial", hasMore: true });
      return { ...queue, capabilities: { ...queue.capabilities, queueRemove: "unsupported", queueReorder: "unsupported" } };
    };
    const { bridge } = createPreloadBridge({ snapshot: tail });
    bridge.sendPlayerCommand.mockImplementation((cmd) => ({ ...accepted(cmd), status: "failed" }) as never);
    renderApp(bridge, "queue");
    const rows = await queueRows();
    expect(within(rows[3]!).queryByTestId("queue-remove")).toBeNull();
    const clear = screen.getByTestId("queue-clear-upcoming") as HTMLButtonElement;
    expect(clear.disabled).toBe(false);
    expect(clear.title).toBe(en.queue.clearHint);
    expect(screen.queryByTestId("queue-note")).toBeNull();
    await fireEvent.click(clear);
    const cmd = bridge.sendPlayerCommand.mock.lastCall![0];
    expect([cmd.op, cmd.args]).toEqual(["clearUpcoming", { queueEntryId: "qe-cur", queueRevision: 7, trackInstanceId: "ti-q" }]);
    cleanup();

    for (const unanchored of [
      // The player is on another occurrence than the queue's current entry.
      (s: AppSnapshot) => ({ ...tail(s), player: { ...tail(s).player, currentQueueEntryId: "qe-b" } }),
      // The current entry is not among the loaded entries.
      (s: AppSnapshot) => {
        const queue = tail(s);
        return { ...queue, player: { ...queue.player, currentQueueEntryId: "qe-gone" }, queueSummary: { ...queue.queueSummary, currentEntryId: "qe-gone" } };
      },
      (s: AppSnapshot) => ({ ...tail(s), player: { ...tail(s).player, trackInstanceId: null } }),
    ]) {
      const next = createPreloadBridge({ snapshot: unanchored });
      renderApp(next.bridge, "queue");
      await queueRows();
      const button = screen.getByTestId("queue-clear-upcoming") as HTMLButtonElement;
      expect(button.disabled).toBe(true);
      expect(button.title).toBe(en.queue.editNeedsCurrent);
      cleanup();
    }

    // Neither clearing nor single edits: nothing to offer.
    const none = createPreloadBridge({
      snapshot: (s) => {
        const queue = tail(s);
        return { ...queue, capabilities: { ...queue.capabilities, queueClear: "unsupported" } };
      },
    });
    renderApp(none.bridge, "queue");
    await queueRows();
    expect(screen.queryByTestId("queue-clear-upcoming")).toBeNull();
  });

  it("disables play without a loaded track but keeps pause available", async () => {
    const fake = new FakeCoreClient("normal");
    const snap = fake.subscribeSnapshot(() => {}).snapshot;
    const overlay = { pendingCount: 0, activeOp: null, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };
    const props = { pendingOverlay: overlay, interpolatedPositionMs: 0, onCommand: vi.fn(async () => null), onOpenNowPlaying: () => {}, onToggleLyrics: () => {} };
    const { rerender } = render(PlayerBar, { props: { ...props, snapshot: { ...snap, player: { ...snap.player, currentTrack: null, playback: "paused" } } } });
    const play = screen.getByTestId("btn-play-pause") as HTMLButtonElement;
    expect(play.disabled).toBe(true);
    expect(play.title).toContain("No track is loaded");
    // Right after launch the core holds a play until the last track is back: the press is allowed.
    await rerender({ snapshot: { ...snap, player: { ...snap.player, currentTrack: null, playback: "idle", preparing: true } } });
    expect(play.disabled).toBe(false);
    expect(play.title).toBe(en.player.preparingTrack);

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
    const cacheFailed = await screen.findByTestId("settings-cache-failed");
    expect(cacheFailed.textContent).toContain("Couldn’t clear the cache");
    expect(cacheFailed.textContent).not.toContain("core busy");
    expect(cacheFailed.getAttribute("title")).toBe("core busy");
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
    // A sticky toast in plain words; the transport's own message is only its tooltip.
    const errorDetail = () =>
      screen.getByTestId("toast-connection-error").querySelector(".toast-text")?.getAttribute("title") ?? "";
    expect(await screen.findByTestId("toast-connection-error")).toBeTruthy();
    expect(errorDetail()).toContain("daemon handshake not ready");
    await waitFor(() => expect(screen.queryByTestId("toast-connection-error")).toBeNull());
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
      expect(errorDetail()).toContain("sidecar crashed during boot");
    } finally {
      vi.useRealTimers();
    }

    allowManual = true;
    await fireEvent.click(screen.getByTestId("btn-reconnect-core"));
    await waitFor(() => expect(screen.queryByTestId("toast-connection-error")).toBeNull());
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
    expect(errBanner.textContent).toContain(en.states.requestTimeout);
    expect(errBanner.textContent).not.toContain("TIMEOUT");
    expect(errBanner.title).toBe(`TIMEOUT${en.common.colon}${en.transport.timeout}`);

    await fireEvent.click(screen.getByTestId("playlist-picker-load-more"));
    await fireEvent.click(await screen.findByTestId("pick-playlist-pl-p2"));
    await waitFor(() => expect(screen.getByTestId("toast-write-done").textContent).toContain("Added to"));
    expect(sendMutation).toHaveBeenLastCalledWith(
      expect.objectContaining({ kind: "addToPlaylist", playlistId: "pl-p2", confirmedByUser: true }),
      "mock-account-epoch-1"
    );
  });

  it("cross-fades a page change inside the scroll region only, never as a document transition that would capture the glass", async () => {
    // The update runs later, as in Chromium: focus has to follow the new page once it is in.
    const scoped = vi.fn(function (this: HTMLElement, cb: () => void | Promise<void>) {
      const updateCallbackDone = new Promise<void>((done) => setTimeout(() => void Promise.resolve(cb()).then(done), 20));
      return { updateCallbackDone };
    });
    const documentWide = vi.fn();
    const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
    const docRecord = document as unknown as Record<string, unknown>;
    const origDoc = Object.getOwnPropertyDescriptor(document, "startViewTransition");
    Object.defineProperty(proto, "startViewTransition", { value: scoped, configurable: true, writable: true });
    Object.defineProperty(document, "startViewTransition", { value: documentWide, configurable: true, writable: true });
    try {
      renderApp(createPreloadBridge().bridge, "home");
      await screen.findByTestId("view-home");
      await fireEvent.click(screen.getByTestId("nav-settings"));
      await screen.findByTestId("view-settings");
      await waitFor(() => expect(document.activeElement?.id).toBe("settings-heading"));
      expect(scoped).toHaveBeenCalledTimes(1);
      expect(scoped.mock.contexts[0]).toBe(screen.getByTestId("main-scroll-region"));
      expect(documentWide).not.toHaveBeenCalled();
    } finally {
      Reflect.deleteProperty(proto, "startViewTransition");
      if (origDoc) Object.defineProperty(document, "startViewTransition", origDoc);
      else Reflect.deleteProperty(docRecord, "startViewTransition");
    }
  });

  it("focuses the search input after async view transitions, without transitions, and under reduced motion", async () => {
    // Page changes start a transition scoped to the scroll region, so the stub lives on elements.
    const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
    const setStartViewTransition = (fn: ((cb: () => void | Promise<void>) => unknown) | undefined) => {
      Object.defineProperty(proto, "startViewTransition", { value: fn, configurable: true, writable: true });
    };
    try {
      const asyncSpy = vi.fn((cb: () => void | Promise<void>) => {
        const updateCallbackDone = new Promise<void>((resolve) => {
          setTimeout(() => {
            void Promise.resolve(cb()).then(resolve);
          }, 20);
        });
        return { updateCallbackDone };
      });
      setStartViewTransition(asyncSpy);
      renderApp(createPreloadBridge().bridge, "home");
      await screen.findByTestId("view-home");
      await fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
      await waitFor(() => expect(document.activeElement?.id).toBe("ytm-search-input"));
      expect(asyncSpy).toHaveBeenCalledTimes(1);
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
      Reflect.deleteProperty(proto, "startViewTransition");
    }
  });

  it("preserves a visible tabindex=0 row after scrolling 1,000 tracks without stealing focus", async () => {
    const tracks = createSyntheticTracks(1000, "kb");
    const entryIds = tracks.map((_, idx) => `qe-kb-${idx}`);
    const onPlayTrack = vi.fn();
    // Rows scroll with the page: render inside the app's scroll container.
    const main = document.body.appendChild(document.createElement("main"));
    main.dataset.testid = "main-scroll-region";
    Object.defineProperty(main, "clientHeight", { value: 480, configurable: true });
    render(VirtualizedTrackList, {
      target: main,
      props: {
        tracks,
        entryIds,
        mockMode: true,
        onPlayTrack,
      },
    });

    const list = screen.getByTestId("virtual-track-list");
    main.scrollTop = 5800;
    await fireEvent.scroll(main);

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
    main.remove();
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

    await screen.findByTestId("view-now-playing");
    const seekSlider = screen.getByTestId("player-seek-slider") as HTMLInputElement;
    const beforePos = screen.getByTestId("player-pos-text").textContent;
    await fireEvent.input(seekSlider, { target: { value: "90000" } });
    expect(screen.getByTestId("player-pos-text").textContent?.trim()).toBe("1:30");
    await fireEvent.change(seekSlider, { target: { value: "90000" } });
    await waitFor(() => {
      expect(screen.getByTestId("player-pos-text").textContent).toBe(beforePos);
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

describe("Lyrics toggle and return frame", () => {
  const onView = (name: string) => screen.findByTestId(`view-${name}`);
  const escape = () => fireEvent.keyDown(document.body, { key: "Escape" });
  const ctrlL = (target: Element = document.body) => fireEvent.keyDown(target, { key: "l", ctrlKey: true });
  const focusedRow = () => document.activeElement?.getAttribute("data-row-index");

  it("closes lyrics from the dock or Ctrl+L back to the page it came from, without history ping-pong", async () => {
    const { bridge } = createPreloadBridge();
    renderApp(bridge, "home");
    await onView("home");
    const dock = screen.getByTestId("btn-open-lyrics");
    expect(dock.getAttribute("aria-pressed")).toBe("false");

    await fireEvent.click(dock);
    await onView("lyrics");
    expect(dock.getAttribute("aria-pressed")).toBe("true");
    expect(dock.classList.contains("btn-icon-active")).toBe(true);
    expect(dock.title).toBe(en.nowPlaying.closeLyrics);
    await fireEvent.click(dock);
    await onView("home");
    expect(dock.getAttribute("aria-pressed")).toBe("false");
    // Closing went back rather than forward: Escape has nowhere left to go.
    await escape();
    await onView("home");

    await ctrlL();
    await onView("lyrics");
    await ctrlL();
    await onView("home");

    // Escape closes a toggled lyrics page the same way.
    await ctrlL();
    await onView("lyrics");
    await escape();
    await onView("home");
    await escape();
    await onView("home");

    // The sidebar entry only navigates; the dock then returns to the page before it.
    await fireEvent.click(screen.getByTestId("nav-search"));
    await onView("search");
    await fireEvent.click(screen.getByTestId("nav-lyrics"));
    await onView("lyrics");
    await fireEvent.click(screen.getByTestId("nav-lyrics"));
    await onView("lyrics");
    await fireEvent.click(dock);
    await onView("search");
  });

  it("leaves an initial lyrics page for Now Playing with a track, else Home", async () => {
    const { bridge } = createPreloadBridge();
    renderApp(bridge, "lyrics");
    await onView("lyrics");
    await fireEvent.click(screen.getByTestId("btn-open-lyrics"));
    await onView("now-playing");
    // Replaced, not pushed: Escape does not bring lyrics back.
    await escape();
    await onView("now-playing");
    cleanup();

    const idle = createPreloadBridge({ snapshot: (s) => ({ ...s, player: { ...s.player, currentTrack: null } }) });
    renderApp(idle.bridge, "lyrics");
    await onView("lyrics");
    await ctrlL();
    await onView("home");
  });

  it("closes an open track menu on Escape before leaving lyrics, even with focus outside it", async () => {
    const { bridge } = createPreloadBridge();
    renderApp(bridge, "home");
    await onView("home");
    // A menu opened on Home stays open across Ctrl+L, so it is still open on the lyrics page.
    await fireEvent.click((await screen.findAllByTestId("track-actions-btn"))[0]!);
    const dialog = await screen.findByTestId("track-actions-menu");
    await ctrlL();
    await onView("lyrics");
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
    (document.activeElement as HTMLElement).blur();

    await escape();
    await waitFor(() => expect(screen.queryByTestId("track-actions-menu")).toBeNull());
    expect(screen.getByTestId("view-lyrics")).toBeTruthy();
    await escape();
    await onView("home");
  });

  it("keeps the Queue page's Played section open across lyrics", async () => {
    const { bridge } = createPreloadBridge({
      snapshot: (s) => {
        const previewEntries = createSyntheticTracks(3, "lq").map((track, i) => ({
          entryId: `lq-${i}`, videoId: track.videoId, track, availability: "available" as const,
        }));
        return {
          ...s,
          player: { ...s.player, currentQueueEntryId: "lq-1", trackInstanceId: "ti-lq" },
          queueSummary: { queueRevision: 1, currentEntryId: "lq-1", completeness: "complete", hasMore: false, loadedCount: 3, previewEntries },
        };
      },
    });
    renderApp(bridge, "queue");
    const toggle = await screen.findByTestId("queue-played-toggle");
    await fireEvent.click(toggle);
    await ctrlL();
    await onView("lyrics");
    await ctrlL();
    await onView("queue");
    expect(screen.getByTestId("queue-played-toggle").getAttribute("aria-expanded")).toBe("true");
  });

  it("puts a list page back where it was scrolled and focused, but not after an account switch", async () => {
    const { bridge, snapshot, emit } = createPreloadBridge();
    renderApp(bridge, "liked");
    await screen.findAllByTestId("virtual-track-row");
    const main = screen.getByTestId("main-scroll-region");
    // jsdom has no layout: the list starts 240 px into the page and moves up as the page scrolls.
    const LIST_TOP = 240;
    const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const top = this.classList.contains("track-list-body") ? LIST_TOP - main.scrollTop : 0;
      return DOMRect.fromRect({ x: 0, y: top, width: 800, height: 0 });
    });
    const scrolled = LIST_TOP + 5 * 56 + 10;

    async function leaveForLyrics() {
      main.scrollTop = scrolled;
      await fireEvent.scroll(main);
      const list = screen.getByTestId("virtual-track-list");
      await waitFor(() => expect(list.querySelector('[data-row-index="7"]')).not.toBeNull());
      const row = list.querySelector<HTMLElement>('[data-row-index="7"]')!;
      row.focus();
      await ctrlL(row);
      await onView("lyrics");
      // A page opened forward starts at its top; the page left keeps its offset for the way back.
      expect(main.scrollTop).toBe(0);
    }

    try {
      await leaveForLyrics();
      await ctrlL();
      await onView("liked");
      await waitFor(() => expect(focusedRow()).toBe("7"));
      expect(main.scrollTop).toBe(scrolled);

      await leaveForLyrics();
      emit({
        deltaType: "authChanged",
        revision: snapshot.revision + 1,
        auth_state: { ...snapshot.authState, accountEpoch: "acct-2", accountScope: "scope-acct-2" },
      });
      await ctrlL();
      await onView("liked");
      await screen.findAllByTestId("virtual-track-row");
      await new Promise((r) => setTimeout(r, 20));
      expect(main.scrollTop).toBe(0);
      expect(focusedRow() ?? null).toBeNull();
    } finally {
      rect.mockRestore();
    }
  });
});

describe("Feedback at the control: likes, covers and lyrics across a track change", () => {
  /** A write path whose results the test releases one by one. */
  function controlledWrites() {
    const calls: { req: MutationRequest; settle: (result: MutationResult) => void }[] = [];
    const sendMutation = vi.fn(
      (req: MutationRequest) => new Promise<MutationResult>((resolve) => calls.push({ req, settle: resolve }))
    );
    return { calls, sendMutation };
  }
  const confirmed: MutationResult = { kind: "setLike", outcome: "confirmed", error: null };

  it("turns the heart at the click as pending, and puts it back with a toast when the write is refused", async () => {
    const { calls, sendMutation } = controlledWrites();
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    const heart = await screen.findByTestId("np-like");
    expect(heart.getAttribute("aria-pressed")).toBe("true");

    await fireEvent.click(heart);
    expect(heart.getAttribute("aria-pressed")).toBe("false");
    expect(heart.getAttribute("aria-busy")).toBe("true");
    expect(screen.getByTestId("player-like").getAttribute("aria-pressed")).toBe("false");

    calls[0]!.settle({
      kind: "setLike",
      outcome: "rejected",
      error: { code: "BUSY", retriable: false, retryAfterMs: null, safeMessage: "no", requestId: null },
    });
    await screen.findByTestId("toast-write-failed");
    expect(heart.getAttribute("aria-pressed")).toBe("true");
    expect(heart.getAttribute("aria-busy")).toBe("false");
  });

  it("ends on the listener's last click when clicks arrive while a write is out, and an older result never overrides it", async () => {
    const { calls, sendMutation } = controlledWrites();
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    const heart = await screen.findByTestId("np-like");

    await fireEvent.click(heart); // unlike
    await fireEvent.click(heart); // like again
    expect(heart.getAttribute("aria-pressed")).toBe("true");
    expect(calls).toHaveLength(1);

    // The unlike is confirmed; the heart keeps the newer click and that is written next.
    calls[0]!.settle(confirmed);
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls.map((c) => c.req.liked)).toEqual([false, true]);
    expect(heart.getAttribute("aria-pressed")).toBe("true");
    expect(heart.getAttribute("aria-busy")).toBe("true");

    calls[1]!.settle(confirmed);
    await waitFor(() => expect(heart.getAttribute("aria-busy")).toBe("false"));
    expect(heart.getAttribute("aria-pressed")).toBe("true");
    expect(sendMutation).toHaveBeenCalledTimes(2);
  });

  it("ends a row's heart on the last click when it is clicked again while its write is out", async () => {
    const { calls, sendMutation } = controlledWrites();
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "liked");
    const heart = (await screen.findAllByTestId("track-like-btn"))[0]!;
    expect(heart.getAttribute("aria-pressed")).toBe("true");
    await fireEvent.click(heart);
    await fireEvent.click(heart);
    expect(heart.getAttribute("aria-pressed")).toBe("true");
    expect(heart.getAttribute("aria-busy")).toBe("true");
    expect(heart.hasAttribute("aria-disabled")).toBe(false);
    calls[0]!.settle(confirmed);
    await waitFor(() => expect(calls).toHaveLength(2));
    calls[1]!.settle(confirmed);
    await waitFor(() => expect(heart.getAttribute("aria-busy")).toBe("false"));
    expect(calls.map((c) => c.req.liked)).toEqual([false, true]);
    expect(heart.getAttribute("aria-pressed")).toBe("true");
  });

  it("sends what the play button shows from Space, and shows it at once", async () => {
    const { bridge } = createPreloadBridge();
    bridge.sendPlayerCommand.mockImplementation(() => new Promise<CommandReceipt>(() => {}));
    renderApp(bridge, "home");
    const play = await screen.findByTestId("btn-play-pause");
    expect(play.getAttribute("aria-label")).toBe("Play");
    await fireEvent.keyDown(document.body, { key: " ", code: "Space" });
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "play" }));
    expect(play.getAttribute("aria-label")).toBe("Pause");
    await fireEvent.keyDown(document.body, { key: " ", code: "Space" });
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(expect.objectContaining({ op: "pause" }));
    expect(play.getAttribute("aria-label")).toBe("Play");
  });

  it("resolves a toggle's desired playback the way core runs it: buffering pauses, and a second toggle flips the first", async () => {
    const { bridge } = createPreloadBridge({ snapshot: (s) => ({ ...s, player: { ...s.player, playback: "buffering" } }) });
    bridge.sendPlayerCommand.mockImplementation(() => new Promise<CommandReceipt>(() => {}));
    const client = createUiClient({ preloadBridge: bridge });
    await client.connect();
    void client.sendPlayerCommand("toggle");
    expect(client.getState().pendingOverlay.desiredPlayback).toBe("paused");
    void client.sendPlayerCommand("toggle");
    expect(client.getState().pendingOverlay.desiredPlayback).toBe("playing");
  });

  it("moves the shown position at least every 250 ms while playing", async () => {
    const { bridge, emit, snapshot } = createPreloadBridge();
    renderApp(bridge, "home");
    const slider = (await screen.findByTestId("player-seek-slider")) as HTMLInputElement;
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    try {
      emit({
        deltaType: "playerChanged",
        revision: snapshot.revision + 1,
        pending: snapshot.pending,
        player: {
          ...snapshot.player,
          playback: "playing",
          timeAnchor: { ...snapshot.player.timeAnchor, positionMs: 10_100, durationMs: 240_000, playbackRate: 1, extrapolating: true },
        },
      });
      await tick();
      let last = Number(slider.value);
      for (let i = 0; i < 12; i += 1) {
        await vi.advanceTimersByTimeAsync(250);
        const now = Number(slider.value);
        expect(now).toBeGreaterThan(last);
        last = now;
      }
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves the track menu's other items usable while a like for that track is out", async () => {
    const { sendMutation } = controlledWrites();
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-like"));
    await fireEvent.click(screen.getByTestId("np-actions"));
    const disabled = async (id: string) => ((await screen.findByTestId(id)) as HTMLButtonElement).disabled;
    expect(await disabled("action-play")).toBe(false);
    expect(await disabled("action-play-next")).toBe(false);
    expect(await disabled("action-add-to-queue")).toBe(false);
    expect(await disabled("action-like")).toBe(true);
    expect(await disabled("action-add-to-playlist")).toBe(true);
  });

  it("keeps the previous cover in the bar and the light while the new one is cached, then switches once", async () => {
    const track = (videoId: string, assetId: string | null) => ({
      ...createSyntheticTracks(1, videoId)[0]!,
      videoId,
      cover: { assetId, remoteUrl: `https://example.com/${videoId}.jpg` },
    });
    const { bridge, emit, snapshot } = createPreloadBridge({
      snapshot: (s) => ({ ...s, player: { ...s.player, currentTrack: track("a", "asset-a") } }),
    });
    renderApp(bridge, "home");
    const bar = await screen.findByTestId("player-bar");
    const barCover = () => bar.querySelector("img.player-cover")?.getAttribute("src") ?? null;
    const light = () =>
      Array.from(screen.getByTestId("ambient-light").querySelectorAll(".ambient-layer:not([data-leaving]) img")).map((img) =>
        img.getAttribute("src")
      );
    await waitFor(() => expect(barCover()).toBe("app://covers/asset-a"));
    expect(light()).toEqual(["app://covers/asset-a"]);

    let revision = snapshot.revision;
    const play = (t: ReturnType<typeof track>, instance: string) =>
      emit({ deltaType: "playerChanged", revision: ++revision, pending: snapshot.pending, player: { ...snapshot.player, currentTrack: t, trackInstanceId: instance } });

    play(track("b", null), "ti-b");
    await tick();
    expect(barCover()).toBe("app://covers/asset-a");
    expect(bar.querySelector("[data-testid='cover-placeholder']")).toBeNull();
    expect(light()).toEqual(["app://covers/asset-a"]);

    play(track("b", "asset-b"), "ti-b");
    await waitFor(() => expect(barCover()).toBe("app://covers/asset-b"));
    expect(light()).toEqual(["app://covers/asset-b"]);

    // A cover that never arrives gives way to the placeholder after a short wait.
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      play(track("c", null), "ti-c");
      await tick();
      expect(barCover()).toBe("app://covers/asset-b");
      await vi.advanceTimersByTimeAsync(2_100);
      expect(barCover()).toBeNull();
      expect(bar.querySelector("[data-testid='cover-placeholder']")).not.toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps Now Playing's lyrics layout while the next track's lyrics load", async () => {
    const synced = (videoId: string): LyricsResult => ({
      ...DISABLED_LYRICS,
      videoId,
      kind: "synced",
      source: "lrclib",
      syncedLines: [{ timeMs: 0, text: `Line of ${videoId}` }],
    });
    let release!: (value: LyricsResult) => void;
    const queryLyrics = vi.fn((req: { track: { videoId: string } }) =>
      req.track.videoId === "mock-vid-00001"
        ? synced(req.track.videoId)
        : new Promise<LyricsResult>((resolve) => (release = resolve))
    );
    const { bridge, emit, snapshot } = createPreloadBridge({ queryLyrics: queryLyrics as unknown as YtmcpUiBridge["queryLyrics"] });
    renderApp(bridge, "now-playing");
    await screen.findByText("Line of mock-vid-00001");
    const stage = () => document.querySelector(".np-stage")!;
    expect(stage().classList.contains("has-lyrics")).toBe(true);

    const next = { ...snapshot.player.currentTrack!, videoId: "mock-vid-00002", title: "Second" };
    emit({ deltaType: "playerChanged", revision: snapshot.revision + 1, pending: snapshot.pending, player: { ...snapshot.player, currentTrack: next, trackInstanceId: "ti-2" } });
    await waitFor(() => expect(queryLyrics).toHaveBeenCalledTimes(2));
    expect(stage().classList.contains("has-lyrics")).toBe(true);
    expect(screen.queryByText("Line of mock-vid-00001")).toBeNull();

    release(synced("mock-vid-00002"));
    await screen.findByText("Line of mock-vid-00002");
    expect(stage().classList.contains("has-lyrics")).toBe(true);
  });

  it("moves the highlight, the bar and the time together at a lyric line click, through the seek bar's own path", async () => {
    const lyrics: LyricsResult = {
      ...DISABLED_LYRICS,
      kind: "synced",
      source: "lrclib",
      syncedLines: Array.from({ length: 10 }, (_, i) => ({ timeMs: i * 10_000, text: `Line ${i}` })),
    };
    const { bridge } = createPreloadBridge({
      queryLyrics: () => lyrics,
      snapshot: (s) => ({ ...s, player: { ...s.player, timeAnchor: { ...s.player.timeAnchor, positionMs: 0, durationMs: 200_000 } } }),
    });
    // The seek is accepted but not yet observed: the preview holds, as it does for the seek bar.
    bridge.sendPlayerCommand.mockImplementation((cmd) => ({
      commandId: cmd.commandId, op: cmd.op, status: "accepted", acceptedAtMs: 0, observedAtMs: null, deduplicated: false, error: null,
    }));
    renderApp(bridge, "lyrics");
    const list = await screen.findByTestId("lyrics-synced-list");
    await fireEvent.click(screen.getAllByTestId("lyrics-seek-line")[6]!);
    expect(bridge.sendPlayerCommand).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: "seek", args: { positionMs: 60_000, trackInstanceId: "ti-mock-1" } })
    );
    await waitFor(() => expect(list.querySelector("[aria-current=true]")?.getAttribute("data-line-index")).toBe("6"));
    expect(screen.getByTestId("player-pos-text").textContent?.trim()).toBe("1:00");
    expect((screen.getByTestId("player-seek-slider") as HTMLInputElement).value).toBe("60000");
  });
});

describe("Liked songs: Play and Shuffle the whole collection", () => {
  const likedTracks = createSyntheticTracks(3); // the first is the snapshot's current track
  type LikedPage = Extract<CatalogResponsePayload, { resultType: "likedSongs" }>;
  const likedPage = (overrides: Partial<LikedPage> = {}): LikedPage => ({
    resultType: "likedSongs",
    ...paged(likedTracks),
    accountScope: "mock-scope",
    playback: {
      playlistId: "LM-from-core",
      entries: likedTracks.map((_, i) => ({ startIndex: 7 + i, playlistEntryId: `entry-${i}` })),
    },
    ...overrides,
  });

  /** The Liked page with its context; commands are accepted and settle only through `settle`. */
  function likedApp(options: { page?: Partial<LikedPage>; shuffleCap?: "supported" | "unsupported"; shuffle?: boolean } = {}) {
    const fake = new FakeCoreClient("normal");
    const harness = createPreloadBridge({
      snapshot: (s) => ({
        ...s,
        capabilities: { ...s.capabilities, shuffle: options.shuffleCap ?? "supported" },
        player: { ...s.player, shuffle: options.shuffle ?? false },
      }),
      queryCatalog: (req) => (req.kind === "likedSongs" ? likedPage(options.page) : fake.queryCatalog(req)),
    });
    harness.bridge.sendPlayerCommand.mockImplementation((cmd): CommandReceipt => ({
      commandId: cmd.commandId, op: cmd.op, status: "accepted", acceptedAtMs: 0, observedAtMs: null, deduplicated: false, error: null,
    }));
    let revision = harness.snapshot.revision;
    const sent = (op: string) => harness.bridge.sendPlayerCommand.mock.calls.map(([cmd]) => cmd).filter((cmd) => cmd.op === op);
    const settle = (cmd: PlayerCommandPayload, status: CommandReceipt["status"]) =>
      harness.emit({
        deltaType: "commandUpdated",
        revision: ++revision,
        receipt: { commandId: cmd.commandId, op: cmd.op, status, acceptedAtMs: 0, observedAtMs: status === "observed" ? 1 : null, deduplicated: false, error: null },
      });
    renderApp(harness.bridge, "liked");
    return { ...harness, sent, settle };
  }

  it("offers Play and Shuffle only with a fresh context read for the signed-in account, Shuffle only when it can work", async () => {
    likedApp();
    expect(await screen.findByTestId("liked-play")).toBeTruthy();
    expect(screen.getByTestId("liked-shuffle")).toBeTruthy();
    expect(screen.getByTestId("liked-strip-play")).toBeTruthy();
    cleanup();

    likedApp({ shuffleCap: "unsupported" });
    expect(await screen.findByTestId("liked-play")).toBeTruthy();
    expect(screen.queryByTestId("liked-shuffle")).toBeNull();
    cleanup();

    for (const page of [{ playback: null }, { freshness: "stale" as const }, { accountScope: "another-scope" }]) {
      likedApp({ page });
      await screen.findByText(likedTracks[2]!.title);
      expect(screen.queryByTestId("liked-play")).toBeNull();
      expect(screen.queryByTestId("liked-shuffle")).toBeNull();
      expect(screen.queryByTestId("liked-strip-play")).toBeNull();
      cleanup();
    }
  });

  it("plays from the first row with the core's context, once for a double press, and turns shuffle off once started", async () => {
    const { sent, settle } = likedApp({ shuffle: true });
    const play = await screen.findByTestId("liked-play");
    await fireEvent.click(play);
    await fireEvent.click(play);
    await fireEvent.click(screen.getByTestId("liked-shuffle"));
    expect(sent("playCollection")).toHaveLength(1);
    expect(sent("playCollection")[0]!.args).toEqual({
      target: { videoId: likedTracks[0]!.videoId, playlistId: "LM-from-core", startIndex: 7 },
    });
    expect(play.getAttribute("data-pending")).toBe("true");
    expect(play.getAttribute("aria-busy")).toBe("true");
    expect(sent("setShuffle")).toHaveLength(0);

    settle(sent("playCollection")[0]!, "observed");
    await waitFor(() => expect(sent("setShuffle")).toHaveLength(1));
    expect(sent("setShuffle")[0]!.args).toEqual({ shuffle: false });
    expect(play.getAttribute("data-pending")).toBeNull();
  });

  it("turns shuffle on only after the start is observed, never after an unclear start or for an older press", async () => {
    const { sent, settle } = likedApp();
    const shuffle = await screen.findByTestId("liked-shuffle");
    await fireEvent.click(shuffle);
    expect(sent("playCollection")).toHaveLength(1);
    expect(shuffle.getAttribute("data-pending")).toBe("true");
    expect(sent("setShuffle")).toHaveLength(0);

    const first = sent("playCollection")[0]!;
    settle(first, "indeterminate");
    await waitFor(() => expect(shuffle.getAttribute("data-pending")).toBeNull());
    expect(sent("setShuffle")).toHaveLength(0);

    await fireEvent.click(shuffle);
    expect(sent("playCollection")).toHaveLength(2);
    // A late receipt for the first press does not count for the second.
    settle(first, "observed");
    await tick();
    expect(sent("setShuffle")).toHaveLength(0);
    expect(shuffle.getAttribute("data-pending")).toBe("true");

    settle(sent("playCollection")[1]!, "observed");
    await waitFor(() => expect(sent("setShuffle")).toHaveLength(1));
    expect(sent("setShuffle")[0]!.args).toEqual({ shuffle: true });
  });

  it("leaves shuffle alone when it is already on, and sends nothing more once the listener left the page", async () => {
    const { sent, settle } = likedApp({ shuffle: true });
    await fireEvent.click(await screen.findByTestId("liked-shuffle"));
    settle(sent("playCollection")[0]!, "observed");
    await waitFor(() => expect(screen.getByTestId("liked-shuffle").getAttribute("data-pending")).toBeNull());
    expect(sent("setShuffle")).toHaveLength(0);
    cleanup();

    const away = likedApp();
    await fireEvent.click(await screen.findByTestId("liked-shuffle"));
    await fireEvent.click(screen.getByTestId("nav-home"));
    await screen.findByTestId("view-home");
    away.settle(away.sent("playCollection")[0]!, "observed");
    await tick();
    await new Promise((r) => setTimeout(r, 20));
    expect(away.sent("setShuffle")).toHaveLength(0);
  });
});

describe("Undo after adding to a playlist", () => {
  const error = (code: string, retriable = false) => ({ code, retriable, retryAfterMs: null, safeMessage: `fixture ${code}`, requestId: null }) as MutationResult["error"];

  /** Adds the current track to Late-Night from Now Playing; `undo` answers the Undo. */
  async function addWithUndo(undo: (req: MutationRequest) => MutationResult | Promise<MutationResult>, add?: MutationResult) {
    const sendMutation = vi.fn(async (req: MutationRequest): Promise<MutationResult> =>
      req.kind === "addToPlaylist"
        ? (add ?? { kind: "addToPlaylist", outcome: "confirmed", error: null, undoToken: "undo-token-1" })
        : undo(req)
    );
    const harness = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(harness.bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-add-to-playlist"));
    await fireEvent.click(await screen.findByTestId("pick-playlist-mock-pl-late-night"));
    await waitFor(() => expect(screen.getByTestId("toast-write-done").textContent).toContain("Added to"));
    const undoCalls = () => sendMutation.mock.calls.filter(([req]) => req.kind === "undoAddToPlaylist");
    return { ...harness, sendMutation, undoCalls };
  }

  it("offers Undo only with a token, sends the token once and reports the removal", async () => {
    let answer: ((result: MutationResult) => void) | null = null;
    const { undoCalls } = await addWithUndo(() => new Promise((resolve) => (answer = resolve)));
    await fireEvent.click(screen.getByTestId("toast-undo"));
    expect(undoCalls()).toEqual([[
      { kind: "undoAddToPlaylist", videoId: "mock-vid-00001", playlistId: "mock-pl-late-night", undoToken: "undo-token-1", liked: null, confirmedByUser: true },
      "mock-account-epoch-1",
    ]]);
    // While it is on its way, Undo stays on the toast, pending, and cannot send again.
    const pending = screen.getByTestId("toast-undo") as HTMLButtonElement;
    expect(pending.disabled).toBe(true);
    expect(pending.getAttribute("data-pending")).toBe("true");
    await fireEvent.click(pending);
    expect(undoCalls()).toHaveLength(1);

    answer!({ kind: "undoAddToPlaylist", outcome: "confirmed", error: null });
    await waitFor(() => expect(screen.getByTestId("toast-undo-done").textContent).toContain("Removed from"));
    expect(screen.queryByTestId("toast-undo")).toBeNull();
    // A successful add and its undo are not problems.
    await fireEvent.click(screen.getByTestId("nav-settings"));
    expect(screen.queryByTestId("settings-problem-count")).toBeNull();
  });

  it.each([
    ["already removed", { kind: "undoAddToPlaylist", outcome: "rejected", error: error("STALE_CONTEXT"), alreadyRemoved: true }, "toast-undo-already-removed", "Already removed"],
    ["a used or expired token", { kind: "undoAddToPlaylist", outcome: "rejected", error: error("STALE_CONTEXT") }, "toast-undo-expired", "Can no longer undo"],
    ["a mismatched token (the core's error)", { kind: "undoAddToPlaylist", outcome: "rejected", error: error("INVALID_PAYLOAD") }, "toast-write-failed", "Couldn’t remove"],
    ["an unclear removal", { kind: "undoAddToPlaylist", outcome: "indeterminate", error: error("TIMEOUT") }, "toast-write-unknown", "Couldn’t confirm the change"],
  ] as const)("reports %s and offers no Undo again", async (_, result, testId, text) => {
    const { undoCalls } = await addWithUndo(() => result as MutationResult);
    await fireEvent.click(screen.getByTestId("toast-undo"));
    await waitFor(() => expect(screen.getByTestId(testId).textContent).toContain(text));
    expect(screen.queryByTestId("toast-undo")).toBeNull();
    expect(undoCalls()).toHaveLength(1);
  });

  it("logs an unclear removal like other unknown writes", async () => {
    await addWithUndo(() => ({ kind: "undoAddToPlaylist", outcome: "indeterminate", error: error("TIMEOUT") }));
    await fireEvent.click(screen.getByTestId("toast-undo"));
    await screen.findByTestId("toast-write-unknown");
    await fireEvent.click(screen.getByTestId("nav-settings"));
    expect((await screen.findByTestId("settings-problem-count")).textContent).toContain("1");
  });

  it("keeps Undo after a refusal that sent nothing and may work later, without retrying by itself", async () => {
    let calls = 0;
    const { undoCalls } = await addWithUndo(() =>
      ++calls === 1
        ? { kind: "undoAddToPlaylist", outcome: "rejected", error: error("RATE_LIMITED", true) }
        : { kind: "undoAddToPlaylist", outcome: "confirmed", error: null }
    );
    await fireEvent.click(screen.getByTestId("toast-undo"));
    await waitFor(() => expect(screen.getByTestId("toast-undo-not-now").textContent).toContain("right now"));
    await new Promise((r) => setTimeout(r, 20));
    expect(undoCalls()).toHaveLength(1);
    await fireEvent.click(screen.getByTestId("toast-undo"));
    await screen.findByTestId("toast-undo-done");
    expect(undoCalls()).toHaveLength(2);
    expect(undoCalls()[1]![0]).toMatchObject({ undoToken: "undo-token-1" });
  });

  it("gives no Undo for an addition without a token or a refused duplicate", async () => {
    await addWithUndo(() => { throw new Error("no undo expected"); }, { kind: "addToPlaylist", outcome: "confirmed", error: null });
    expect(screen.queryByTestId("toast-undo")).toBeNull();
    cleanup();

    const sendMutation = vi.fn(async (): Promise<MutationResult> => ({ kind: "addToPlaylist", outcome: "rejected", error: error("UNAVAILABLE_TRACK") }));
    const { bridge } = createPreloadBridge({ snapshot: writeReady, sendMutation });
    renderApp(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-add-to-playlist"));
    await fireEvent.click(await screen.findByTestId("pick-playlist-mock-pl-late-night"));
    await screen.findByTestId("toast-write-failed");
    expect(screen.queryByTestId("toast-undo")).toBeNull();
  });

  it.each([
    ["a confirmed removal", { kind: "undoAddToPlaylist", outcome: "confirmed", error: null }],
    ["a refusal that may work later", { kind: "undoAddToPlaylist", outcome: "rejected", error: error("RATE_LIMITED", true) }],
  ] as const)("says nothing of an undo answered after the account changed (%s), and offers no Undo", async (_, result) => {
    let answer: ((result: MutationResult) => void) | null = null;
    const { emit, snapshot, undoCalls } = await addWithUndo(() => new Promise((resolve) => (answer = resolve)));
    await fireEvent.click(screen.getByTestId("toast-undo"));
    emit({
      deltaType: "authChanged",
      revision: snapshot.revision + 1,
      auth_state: { ...snapshot.authState, state: "authenticated", accountEpoch: "acct-2", accountScope: "scope-acct-2", playerCatalogMatched: true },
    });
    await waitFor(() => expect(screen.queryByTestId("toast-undo")).toBeNull());
    answer!(result as MutationResult);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByTestId("toast-undo-done")).toBeNull();
    expect(screen.queryByTestId("toast-undo-not-now")).toBeNull();
    expect(screen.queryByTestId("toast-undo")).toBeNull();
    expect(undoCalls()).toHaveLength(1);
  });

  it("drops the token and its Undo after five minutes", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const { undoCalls } = await addWithUndo(() => ({ kind: "undoAddToPlaylist", outcome: "confirmed", error: null }));
      const toast = screen.getByTestId("toast-write-done");
      await fireEvent.pointerEnter(toast); // held under the pointer, the toast itself would stay
      await vi.advanceTimersByTimeAsync(5 * 60 * 1000 + 10);
      await waitFor(() => expect(screen.queryByTestId("toast-undo")).toBeNull());
      expect(undoCalls()).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
