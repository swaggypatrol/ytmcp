import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import {
  FakeCoreClient,
  createSyntheticTracks,
  type AppSnapshot,
  type CatalogQueryRequest,
  type CatalogResponsePayload,
  type LyricsResult,
  type PlayerCommandPayload,
  type StateDelta,
  type Track,
} from "@ytmcp/protocol";
import { tick } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../App.svelte";
import { createUiClient, type YtmcpUiBridge } from "../../lib/ipc/client.js";
import type { NavView } from "../../lib/i18n/en.js";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const NO_LYRICS: LyricsResult = {
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

interface Held {
  readonly req: CatalogQueryRequest;
  readonly resolve: () => void;
}

/**
 * A preload bridge over the fake core. `hold(req)` true keeps that catalog reply back until the
 * test releases it, the stand-in for a slow network.
 */
function preload(options: { snapshot?: (s: AppSnapshot) => AppSnapshot; hold?: (req: CatalogQueryRequest) => boolean } = {}) {
  const fake = new FakeCoreClient("normal");
  const base: AppSnapshot = { ...fake.subscribeSnapshot(() => {}).snapshot, mockMode: false };
  const snapshot = options.snapshot ? options.snapshot(base) : base;
  let listener: ((delta: StateDelta) => void) | null = null;
  const calls: CatalogQueryRequest[] = [];
  const held: Held[] = [];
  const bridge = {
    subscribeSnapshot: (l: (delta: StateDelta) => void) => {
      listener = l;
      return { snapshot: structuredClone(snapshot), unsubscribe: () => (listener = null) };
    },
    sendPlayerCommand: vi.fn((cmd: PlayerCommandPayload) => fake.sendPlayerCommand(cmd)),
    queryCatalog: vi.fn((req: CatalogQueryRequest) => {
      calls.push(req);
      if (!options.hold?.(req)) return fake.queryCatalog(req);
      return new Promise<CatalogResponsePayload>((resolve) => held.push({ req, resolve: () => resolve(fake.queryCatalog(req)) }));
    }),
    queryLyrics: () => NO_LYRICS,
    updateSettings: vi.fn(),
    momentSettings: vi.fn((req: Parameters<FakeCoreClient["momentSettings"]>[0]) => fake.momentSettings(req)),
    openPlayerWindow: vi.fn(),
    clearCache: vi.fn(),
    logout: vi.fn(),
  } satisfies YtmcpUiBridge;
  return { bridge, fake, snapshot, calls, held, emit: (d: StateDelta) => listener?.(d) };
}

const renderApp = (bridge: YtmcpUiBridge, initialView: NavView = "home") =>
  render(App, { props: { client: createUiClient({ preloadBridge: bridge }), initialView } });

const main = () => screen.getByTestId("main-scroll-region");
const escape = () => fireEvent.keyDown(document.body, { key: "Escape" });
const settle = () => new Promise((r) => setTimeout(r, 20));

describe("Page scroll across navigation", () => {
  it("opens every page at its top and puts each one back where it was on Back", async () => {
    const { bridge } = preload();
    renderApp(bridge, "home");
    await screen.findByTestId("view-home");
    main().scrollTop = 600;

    await fireEvent.click(screen.getByTestId("nav-settings"));
    await screen.findByTestId("view-settings");
    expect(main().scrollTop).toBe(0);
    main().scrollTop = 300;

    await fireEvent.click(screen.getByTestId("nav-queue"));
    await screen.findByTestId("view-queue");
    expect(main().scrollTop).toBe(0);

    await escape();
    await screen.findByTestId("view-settings");
    expect(main().scrollTop).toBe(300);
    await escape();
    await screen.findByTestId("view-home");
    expect(main().scrollTop).toBe(600);
  });
});

describe("Back to a page that has to load again", () => {
  /** The main region clamps its offset as a browser does: nothing to scroll until the cards are there. */
  function clampedMain() {
    const el = main();
    let top = 0;
    Object.defineProperty(el, "scrollTop", {
      configurable: true,
      get: () => top,
      set: (value: number) => {
        const max = el.querySelector('[data-testid^="playlist-card-"]') ? 5_000 : 0;
        top = Math.max(0, Math.min(value, max));
      },
    });
  }

  /** Library scrolled to 600, Settings opened, and the library dropped meanwhile (an add to a playlist). */
  async function leaveLibrary() {
    let holding = false;
    const setup = preload({ hold: (req) => holding && req.kind === "libraryPlaylists" });
    renderApp(setup.bridge, "library");
    await screen.findAllByTestId(/^playlist-card-/);
    clampedMain();
    main().scrollTop = 600;
    await fireEvent.click(screen.getByTestId("nav-settings"));
    await screen.findByTestId("view-settings");
    holding = true;
    setup.emit({ deltaType: "resourceInvalidated", revision: setup.snapshot.revision + 1, resource_kind: "playlistMembership" });
    await escape();
    await screen.findByTestId("view-library");
    await waitFor(() => expect(setup.held).toHaveLength(1));
    expect(screen.queryAllByTestId(/^playlist-card-/)).toHaveLength(0);
    return setup;
  }

  it("puts the saved offset back once the page's content is there", async () => {
    const { held } = await leaveLibrary();
    held.splice(0).forEach((h) => h.resolve());
    await screen.findAllByTestId(/^playlist-card-/);
    await waitFor(() => expect(main().scrollTop).toBe(600));
  });

  it("leaves the page where the listener scrolled it before the content came", async () => {
    const { held } = await leaveLibrary();
    await fireEvent.wheel(main(), { deltaY: 40 });
    held.splice(0).forEach((h) => h.resolve());
    await screen.findAllByTestId(/^playlist-card-/);
    await settle();
    expect(main().scrollTop).toBe(0);
  });
});

describe("Collections while their data loads", () => {
  const isPlaylist = (req: CatalogQueryRequest) => req.kind === "playlistTracks";

  it("shows a recently opened playlist again at once on Back, and a new one's header from its summary", async () => {
    let holding = false;
    const { bridge, fake, held } = preload({ hold: (req) => holding && isPlaylist(req) });
    const library = fake.queryCatalog({ kind: "libraryPlaylists" });
    if (library.resultType !== "libraryPlaylists") throw new Error("missing Library fixture");
    const [first, second] = library.items;
    renderApp(bridge, "library");

    await fireEvent.click(await screen.findByTestId(`playlist-card-${first!.playlistId}`));
    await screen.findAllByTestId("virtual-track-row");
    await escape();
    await screen.findByTestId("view-library");

    // Not loaded yet: the title and count the library named are there at once, no loading words.
    holding = true;
    await fireEvent.click(screen.getByTestId(`playlist-card-${second!.playlistId}`));
    const page = within(await screen.findByTestId("view-playlist"));
    expect(page.getByRole("heading", { level: 1 }).textContent).toBe(second!.title);
    expect(page.getByTestId("playlist-count").textContent).toContain(String(second!.trackCount));
    expect(page.getByTestId("playlist-state-loading").classList.contains("is-waiting")).toBe(true);
    held.splice(0).forEach((h) => h.resolve());
    await page.findAllByTestId("virtual-track-row");

    // Back to the first one (via the library): its rows are there before its refresh answers.
    await escape();
    await screen.findByTestId("view-library");
    await fireEvent.click(screen.getByTestId(`playlist-card-${first!.playlistId}`));
    const again = within(await screen.findByTestId("view-playlist"));
    expect(again.getByRole("heading", { level: 1 }).textContent).not.toBe(second!.title);
    expect(again.getAllByTestId("virtual-track-row").length).toBeGreaterThan(0);
    expect(again.queryByTestId("playlist-state-loading")).toBeNull();
    expect(held).toHaveLength(1);
    held.splice(0).forEach((h) => h.resolve());
  });

  it("keeps a deep playlist's rows and reads them again in place after an add to a playlist", async () => {
    const { bridge, calls, emit, snapshot } = preload();
    renderApp(bridge, "library");
    await fireEvent.click(await screen.findByTestId("playlist-card-mock-pl-favorites"));
    await screen.findAllByTestId("virtual-track-row");
    // Scrolled to the end: the list pages on; then back up among the rows.
    main().scrollTop = 1_000_000;
    await fireEvent.scroll(main());
    await waitFor(() => expect(calls.some((r) => isPlaylist(r) && r.cursor === "200")).toBe(true));
    main().scrollTop = 5_000;
    await fireEvent.scroll(main());
    await settle();
    const loaded = calls.filter(isPlaylist).map((r) => r.cursor ?? null);
    calls.length = 0;

    // Core names no playlist after an add today: the open one is read again, not cut to 100 rows.
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "playlistTracks" });
    await waitFor(() => expect(calls.filter(isPlaylist).map((r) => r.cursor ?? null)).toEqual(loaded));
    await settle();
    expect(screen.queryByTestId("playlist-state-loading")).toBeNull();
    expect(main().scrollTop).toBe(5_000);
    // The cursor is the refreshed one: paging goes on after the last page read again.
    main().scrollTop = 1_000_000;
    await fireEvent.scroll(main());
    await waitFor(() => expect(calls.filter(isPlaylist)[loaded.length]?.cursor).toBe(String(100 * loaded.length)));
  });

  it("keeps the previous results on screen, marked, until the new query answers", async () => {
    let holding = false;
    const { bridge, held } = preload({ hold: (req) => holding && req.kind === "searchSongs" });
    renderApp(bridge, "search");
    const input = (await screen.findByTestId("search-input")) as HTMLInputElement;
    const submit = async (text: string) => {
      await fireEvent.input(input, { target: { value: text } });
      await fireEvent.submit(input.closest("form")!);
    };
    await submit("Neon");
    await screen.findAllByTestId("virtual-track-row");
    const before = screen.getAllByTestId("virtual-track-row").map((row) => row.textContent);

    holding = true;
    await submit("夜");
    await tick();
    const results = screen.getByTestId("search-results");
    expect(screen.getAllByTestId("virtual-track-row").map((row) => row.textContent)).toEqual(before);
    expect(results.getAttribute("aria-busy")).toBe("true");
    expect(screen.queryByTestId("search-state-loading")).toBeNull();

    held.splice(0).forEach((h) => h.resolve());
    await waitFor(() => expect(results.getAttribute("aria-busy")).toBeNull());
    expect(screen.getAllByTestId("virtual-track-row").every((row) => row.textContent?.includes("夜"))).toBe(true);

    // An earlier "no results" is not shown as the answer to a query still loading.
    holding = false;
    await submit("no such song anywhere");
    await screen.findByTestId("search-state-empty");
    holding = true;
    await submit("Neon");
    await tick();
    expect(screen.queryByTestId("search-state-empty")).toBeNull();
    expect(screen.getByTestId("search-state-loading")).toBeTruthy();
    held.splice(0).forEach((h) => h.resolve());
    await screen.findAllByTestId("virtual-track-row");
  });
});

describe("Snapshot notifications", () => {
  it("does not re-render queue rows for a position tick that changed nothing they show", async () => {
    let reads = 0;
    /** A track whose title read is counted: a row that re-renders reads it again. */
    const counted = (track: Track): Track => {
      const copy = { ...track } as Track & { title: string };
      Object.defineProperty(copy, "title", { get: () => (reads++, track.title), enumerable: true });
      return copy;
    };
    const { bridge, snapshot, emit } = preload();
    renderApp(bridge, "queue");
    await screen.findByTestId("view-queue");
    const previewEntries = createSyntheticTracks(4, "nq").map((track, i) => ({
      entryId: `nq-${i}`, videoId: track.videoId, track: counted(track), availability: "available" as const,
    }));
    emit({
      deltaType: "queueReplaced",
      revision: snapshot.revision + 1,
      queue_summary: { queueRevision: 2, currentEntryId: "nq-0", completeness: "complete", hasMore: false, loadedCount: 4, previewEntries },
    });
    await screen.findByText(createSyntheticTracks(4, "nq")[1]!.title);
    await settle();

    // What the transport delivers each second: a new player object, only the position changed.
    reads = 0;
    for (let i = 0; i < 3; i++) {
      const player = structuredClone(snapshot.player);
      player.timeAnchor = { ...player.timeAnchor, positionMs: 1_000 * (i + 1) };
      emit({ deltaType: "playerChanged", revision: snapshot.revision + 2 + i, player, pending: structuredClone(snapshot.pending) });
      await tick();
    }
    await settle();
    expect(reads).toBe(0);
  });
});

describe("Ambient light while a collection loads", () => {
  it("holds the light it opened under until the collection's art is known, then changes once", async () => {
    const { bridge, held } = preload({
      snapshot: (s) => ({ ...s, mockMode: true }),
      hold: (req) => req.kind === "playlistTracks",
    });
    renderApp(bridge, "home");
    await screen.findByTestId("view-home");
    const light = screen.getByTestId("ambient-light");
    const shown = () =>
      Array.from(light.querySelectorAll(".ambient-layer:not([data-leaving]) img")).map((img) => img.getAttribute("src"));
    await waitFor(() => expect(shown().length).toBeGreaterThan(0));
    const home = shown();
    const layersBefore = light.querySelectorAll(".ambient-layer").length;

    const card = (await screen.findAllByTestId(/^home-playlist-/))[0]!;
    await fireEvent.click(card);
    await screen.findByTestId("view-playlist");
    await settle();
    expect(shown()).toEqual(home);
    expect(light.querySelectorAll(".ambient-layer").length).toBe(layersBefore);

    held.splice(0).forEach((h) => h.resolve());
    await screen.findAllByTestId("virtual-track-row");
    await waitFor(() => expect(shown()).not.toEqual(home));
    expect(shown()).toHaveLength(1);
  });
});
