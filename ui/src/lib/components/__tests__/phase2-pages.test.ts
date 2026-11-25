import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/svelte";
import { createSyntheticTracks, type HomeFeed, type PlaylistSummary, type Track } from "@ytmcp/protocol";
import { createRawSnippet } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../../i18n/index.svelte.js";
import CoverShelf, { alignedOffset, arrowTarget, shelfEndPadding, shelfWheelTarget } from "../CoverShelf.svelte";
import HomeView from "../HomeView.svelte";
import LibraryView from "../LibraryView.svelte";
import PageState from "../PageState.svelte";
import SearchView from "../SearchView.svelte";

const ACCOUNT_A = "acct:scope-a:epoch-1";
const ACCOUNT_B = "acct:scope-b:epoch-2";
const ready = { kind: "ready", cached: false, error: null } as const;

function playlist(id: string): PlaylistSummary {
  return { playlistId: id, title: `List ${id}`, subtitle: null, trackCount: 3, cover: null } as PlaylistSummary;
}

beforeEach(() => {
  setLocale("en");
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("Library paging", () => {
  const props = {
    view: ready,
    playlists: [playlist("p1"), playlist("p2")],
    hasMore: true,
    loadingMore: false,
    moreError: null,
    mockMode: true,
    onOpenPlaylist: () => {},
    onOpenLiked: () => {},
    onOpenRecent: () => {},
    onRetry: () => {},
    onOpenOfficialPlayer: () => {},
  };
  const moreError = { code: "TIMEOUT", retriable: true, retryAfterMs: null, safeMessage: "t", requestId: null } as const;

  it("loads the next page ahead of the end with no Load more button, and retries only from the tail", async () => {
    const onLoadMore = vi.fn();
    const { rerender } = render(LibraryView, { props: { ...props, onLoadMore } });
    await vi.waitFor(() => expect(onLoadMore).toHaveBeenCalled());
    expect(screen.getByTestId("view-library").textContent).not.toMatch(/Load more/);

    await rerender({ ...props, onLoadMore, moreError });
    onLoadMore.mockClear();
    await fireEvent.scroll(window);
    expect(onLoadMore).not.toHaveBeenCalled();
    expect(screen.getByTestId("library-tail").textContent).toContain("Couldn’t load more playlists");
    await fireEvent.click(screen.getByTestId("library-retry"));
    expect(onLoadMore).toHaveBeenCalledTimes(1);

    await rerender({ ...props, onLoadMore, hasMore: false });
    expect(screen.queryByTestId("library-tail")).toBeNull();
  });
});

describe("Browse ruler", () => {
  const css = readFileSync(resolve(import.meta.dirname, "../../../styles/shelf.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

  it("puts the search field 24 under the title", () => {
    expect(css).toMatch(/\.browse-page > \.browse-head \+ \.search-form \{\s*margin-top: var\(--space-24\);/);
  });

  it("sizes shelf cards 152, 168 and 184 px by tier", () => {
    const widths = [...css.matchAll(/\.shelf-card \{[^}]*?width: ([^;]+);/g)].map((m) => m[1]);
    expect(widths).toEqual(["152px", "168px", "184px"]);
  });
});

describe("Cover shelf scrolling", () => {
  const wheel = (deltaY: number, extra: Partial<WheelEvent> = {}) => ({
    deltaX: 0,
    deltaY,
    deltaMode: 0,
    shiftKey: false,
    ctrlKey: false,
    ...extra,
  });

  it("takes a vertical wheel while it can move and hands it back at either end", () => {
    expect(shelfWheelTarget(wheel(100), 0, 500, 900)).toBe(100);
    expect(shelfWheelTarget(wheel(100), 450, 500, 900)).toBe(500);
    expect(shelfWheelTarget(wheel(100), 500, 500, 900)).toBeNull();
    expect(shelfWheelTarget(wheel(-100), 0, 500, 900)).toBeNull();
    // Lines are scaled to pixels.
    expect(shelfWheelTarget(wheel(3, { deltaMode: 1 }), 0, 500, 900)).toBe(120);
  });

  it("leaves sideways gestures and zoom native, and keeps Shift turns even at an end", () => {
    expect(shelfWheelTarget(wheel(10, { deltaX: 40 }), 0, 500, 900)).toBeNull();
    expect(shelfWheelTarget(wheel(100, { ctrlKey: true }), 0, 500, 900)).toBeNull();
    expect(shelfWheelTarget(wheel(100, { shiftKey: true }), 500, 500, 900)).toBe(500);
  });

  it("moves arrows about 80% of the visible width onto a card edge, and settles in the direction of travel", () => {
    const offsets = [0, 184, 368, 552, 736];
    expect(arrowTarget(offsets, 0, 1, 700)).toBe(552);
    expect(arrowTarget(offsets, 552, -1, 700)).toBe(0);
    expect(arrowTarget(offsets, 736, 1, 700)).toBe(736);
    // A card wider than 80% of the view still moves one card.
    expect(arrowTarget(offsets, 0, 1, 100)).toBe(184);
    expect(alignedOffset(offsets, 100, 1)).toBe(184);
    expect(alignedOffset(offsets, 100, -1)).toBe(0);
  });

  it("pads the far end so the last resting place is a card edge and clears the fade", () => {
    // Eight 168 px cards, 16 px apart, in a 700 px view.
    const track = 8 * 168 + 7 * 16;
    const pad = shelfEndPadding(track, 700, 184, 8, 48)!;
    const max = track + pad - 700;
    expect(max % 184).toBe(0);
    expect(pad).toBeGreaterThanOrEqual(48);
    expect(shelfEndPadding(2 * 168 + 16, 700, 184, 2, 48)).toBeNull();
  });

  it("consumes the wheel event only when it scrolls the shelf", async () => {
    vi.useFakeTimers();
    const tiles = ["a", "b", "c"].map((key) => ({ key, title: key, testId: `t-${key}`, onOpen: () => {} }));
    render(CoverShelf, { props: { heading: "Playlists", tiles, mockMode: true, smoothScroll: false } });
    const shelf = screen.getByTestId("cover-shelf") as HTMLDivElement;
    Object.defineProperty(shelf, "scrollWidth", { value: 1000, configurable: true });
    Object.defineProperty(shelf, "clientWidth", { value: 600, configurable: true });
    const scrollTo = vi.fn((opts: ScrollToOptions) => {
      shelf.scrollLeft = opts.left ?? 0;
    });
    shelf.scrollTo = scrollTo as unknown as typeof shelf.scrollTo;

    expect(await fireEvent.wheel(shelf, { deltaY: 100 })).toBe(false);
    expect(scrollTo).toHaveBeenLastCalledWith({ left: 100, behavior: "auto" });
    // The turn ends after a pause; then the shelf is at its far end.
    vi.advanceTimersByTime(1000);
    shelf.scrollLeft = 400;
    scrollTo.mockClear();
    expect(await fireEvent.wheel(shelf, { deltaY: 100 })).toBe(true);
    expect(scrollTo).not.toHaveBeenCalled();
  });
});

describe("Home", () => {
  const feed = (personalizedSupported: boolean): HomeFeed =>
    ({
      recentTracks: createSyntheticTracks(9, "home"),
      pinnedPlaylists: [playlist("p1"), playlist("p2")],
      personalizedSupported,
      fallbackNotice: "Personalized recommendations are not available; showing your library and recent plays",
    }) as unknown as HomeFeed;

  it("presents what exists without a notice about missing recommendations", () => {
    const onOpenRecent = vi.fn();
    render(HomeView, {
      props: {
        view: ready,
        homeFeed: feed(false),
        currentVideoId: null,
        mockMode: true,
        onPlayTrack: () => {},
        onTrackActions: () => {},
        onOpenPlaylist: () => {},
        onOpenRecent,
        onRetry: () => {},
        onOpenOfficialPlayer: () => {},
      },
    });
    const home = screen.getByTestId("view-home");
    expect(home.textContent).not.toMatch(/Personalized|not available/i);
    expect(screen.getByTestId("home-playlist-p1")).toBeTruthy();
    // Six recent plays at most; the rest are one "View all" away.
    expect(screen.getAllByTestId("virtual-track-row").length).toBeLessThanOrEqual(6);
    const viewAll = screen.getByTestId("home-view-all-recent");
    expect(viewAll.textContent?.trim()).toBe("View all");
    viewAll.click();
    expect(onOpenRecent).toHaveBeenCalledOnce();
  });
});

describe("Search before typing", () => {
  function renderSearch(overrides: Record<string, unknown> = {}) {
    const onSearch = vi.fn();
    const onPlayTrack = vi.fn();
    const onOpenAlbum = vi.fn();
    const onRequestStartData = vi.fn();
    const onRememberQuery = vi.fn();
    const props = {
      query: "",
      view: ready,
      songs: [] as Track[],
      albums: [],
      artists: [],
      playlists: [],
      hasMore: false,
      loadingMore: false,
      moreError: null,
      currentVideoId: null,
      mockMode: true,
      blocked: false,
      accountKey: ACCOUNT_A,
      recentTracks: [] as Track[],
      libraryPlaylists: [] as PlaylistSummary[],
      smoothScroll: false,
      onRequestStartData,
      onRememberQuery,
      onSearch,
      onLoadMore: () => {},
      onPlayTrack,
      onTrackActions: () => {},
      onOpenAlbum,
      onOpenArtist: () => {},
      onOpenPlaylist: () => {},
      onRetry: () => {},
      onOpenOfficialPlayer: () => {},
      ...overrides,
    };
    const view = render(SearchView, { props });
    return { ...view, onSearch, onPlayTrack, onOpenAlbum, onRequestStartData, onRememberQuery };
  }

  const startPane = () => screen.getByTestId("search-start");
  const resultsPane = () => screen.getByTestId("search-results");
  // Svelte sets `inert` as a property; jsdom does not reflect it to the attribute.
  const isInert = (el: HTMLElement) => el.hasAttribute("inert") || (el as HTMLElement & { inert?: boolean }).inert === true;

  it("builds its blocks from real data and leaves out a block with nothing in it", async () => {
    const tracks = createSyntheticTracks(4, "recent");
    const unavailable = { ...tracks[3]!, availability: "unavailable" } as Track;
    const { onPlayTrack, onRequestStartData } = renderSearch({
      recentTracks: [tracks[0], tracks[1], tracks[0], tracks[2], unavailable],
      libraryPlaylists: [playlist("lib1")],
    });
    await Promise.resolve();
    expect(onRequestStartData).toHaveBeenCalled();

    expect(screen.queryByTestId("search-recent")).toBeNull();
    const jump = within(startPane()).getAllByTestId(/^search-jump-/);
    expect(jump.map((el) => el.dataset.testid)).toEqual([
      `search-jump-${tracks[0]!.videoId}`,
      `search-jump-${tracks[1]!.videoId}`,
      `search-jump-${tracks[2]!.videoId}`,
    ]);
    jump[1]!.click();
    expect(onPlayTrack).toHaveBeenCalledWith(tracks[1]);
    expect(within(startPane()).getByTestId("search-library-lib1")).toBeTruthy();
  });

  it("asks for the start data again once it has been dropped while the page is open", async () => {
    const { onRequestStartData, rerender } = renderSearch({ startDataMissing: true });
    await Promise.resolve();
    const calls = onRequestStartData.mock.calls.length;
    await rerender({ startDataMissing: false });
    await rerender({ startDataMissing: true });
    await Promise.resolve();
    expect(onRequestStartData.mock.calls.length).toBeGreaterThan(calls + 0);
    await rerender({ accountKey: ACCOUNT_B });
    await Promise.resolve();
    expect(onRequestStartData.mock.calls.length).toBeGreaterThan(calls + 1);
  });

  it("asks for nothing and shows no blocks while search is blocked", async () => {
    const { onRequestStartData } = renderSearch({ blocked: true, recentTracks: createSyntheticTracks(2, "r") });
    await Promise.resolve();
    expect(onRequestStartData).not.toHaveBeenCalled();
    expect(startPane().classList.contains("is-active")).toBe(false);
  });

  it("records only queries the user ran: never debounced typing or text still being composed", async () => {
    vi.useFakeTimers();
    const onClearRecent = vi.fn();
    const { onSearch, onOpenAlbum, onRememberQuery, rerender } = renderSearch({ recentQueries: ["glass", "syn"], onClearRecent });
    const input = screen.getByTestId("search-input") as HTMLInputElement;

    await fireEvent.input(input, { target: { value: "syn" } });
    vi.advanceTimersByTime(300);
    expect(onSearch).toHaveBeenLastCalledWith("syn");
    // An IME composition searches when it ends, and Enter while composing submits nothing.
    await fireEvent.compositionStart(input);
    await fireEvent.input(input, { target: { value: "synt" } });
    await fireEvent.submit(input.closest("form")!);
    await fireEvent.compositionEnd(input, { target: { value: "synth" } });
    vi.advanceTimersByTime(300);
    expect(onSearch).toHaveBeenLastCalledWith("synth");
    expect(onRememberQuery).not.toHaveBeenCalled();

    await fireEvent.submit(input.closest("form")!);
    expect(onRememberQuery).toHaveBeenLastCalledWith("synth");

    // Opening a result keeps the query that found it.
    await rerender({ query: "glass", albums: [{ id: "alb-1", browseId: null, title: "Album" }] });
    await fireEvent.input(input, { target: { value: "glass" } });
    screen.getByTestId("search-album-alb-1").click();
    expect(onOpenAlbum).toHaveBeenCalledWith("alb-1");
    expect(onRememberQuery).toHaveBeenLastCalledWith("glass");

    await fireEvent.input(input, { target: { value: "" } });
    const chips = within(screen.getByTestId("search-recent")).getAllByTestId("search-recent-chip");
    expect(chips.map((c) => c.textContent?.trim())).toEqual(["glass", "syn"]);
    await fireEvent.click(chips[1]!);
    expect(onSearch).toHaveBeenLastCalledWith("syn");
    expect(input.value).toBe("syn");
    expect(onRememberQuery).toHaveBeenLastCalledWith("syn");
    expect(onRememberQuery).toHaveBeenCalledTimes(3);

    await fireEvent.input(input, { target: { value: "" } });
    await fireEvent.click(screen.getByTestId("search-recent-clear"));
    expect(onClearRecent).toHaveBeenCalledTimes(1);
    await rerender({ recentQueries: [] });
    expect(screen.queryByTestId("search-recent")).toBeNull();
  });

  it("holds a list that arrives late until it can appear without pushing the blocks below", async () => {
    const recentTracks = createSyntheticTracks(2, "r");
    const { rerender } = renderSearch({ recentTracks, recentQueries: [] });
    const input = screen.getByTestId("search-input") as HTMLInputElement;
    await rerender({ recentQueries: ["late"] });
    expect(screen.queryByTestId("search-recent")).toBeNull();

    // Out of view (typing), the list takes its place; it is there when the field is cleared.
    await fireEvent.input(input, { target: { value: "x" } });
    await fireEvent.input(input, { target: { value: "" } });
    expect(screen.getByTestId("search-recent").textContent).toContain("late");

    // With nothing below yet, the list shows at once; emptying always shows at once.
    cleanup();
    const second = renderSearch({ recentQueries: [] });
    await second.rerender({ recentQueries: ["first"] });
    expect(screen.getByTestId("search-recent").textContent).toContain("first");
    await second.rerender({ recentTracks, recentQueries: [] });
    expect(screen.queryByTestId("search-recent")).toBeNull();
  });

  it("gives way to results as soon as typing starts and comes back when the field is cleared", async () => {
    renderSearch({ recentQueries: ["earlier"] });
    const input = screen.getByTestId("search-input") as HTMLInputElement;
    expect(startPane().classList.contains("is-active")).toBe(true);
    expect(isInert(resultsPane())).toBe(true);

    await fireEvent.input(input, { target: { value: "n" } });
    expect(startPane().classList.contains("is-active")).toBe(false);
    expect(isInert(startPane())).toBe(true);
    expect(resultsPane().classList.contains("is-active")).toBe(true);

    await fireEvent.click(screen.getByTestId("search-clear-btn"));
    expect(startPane().classList.contains("is-active")).toBe(true);
    expect(isInert(startPane())).toBe(false);
  });
});

describe("Search songs and list rows", () => {
  const base = {
    view: ready,
    albums: [],
    artists: [],
    playlists: [],
    currentVideoId: null,
    mockMode: true,
    blocked: false,
    accountKey: ACCOUNT_A,
    smoothScroll: false,
    onSearch: () => {},
    onPlayTrack: () => {},
    onTrackActions: () => {},
    onOpenAlbum: () => {},
    onOpenArtist: () => {},
    onOpenPlaylist: () => {},
    onRetry: () => {},
    onOpenOfficialPlayer: () => {},
  };
  const like = { liked: () => false, pending: () => false, toggle: vi.fn() };

  it("pages the songs through the list itself, with its own retry, and no Load more button", async () => {
    const onLoadMore = vi.fn();
    const props = { ...base, query: "abc", songs: createSyntheticTracks(3, "s"), hasMore: true, loadingMore: false, moreError: null, onLoadMore };
    const { rerender } = render(SearchView, { props });
    await fireEvent.input(screen.getByTestId("search-input"), { target: { value: "abc" } });
    await fireEvent.click(screen.getByTestId("search-all-songs"));
    await vi.waitFor(() => expect(onLoadMore).toHaveBeenCalled());
    expect(screen.queryByTestId("search-load-more")).toBeNull();

    const moreError = { code: "TIMEOUT", retriable: true, retryAfterMs: null, safeMessage: "t", requestId: null } as const;
    await rerender({ ...props, moreError });
    expect(screen.queryByTestId("search-more-error")).toBeNull();
    await fireEvent.click(screen.getByTestId("track-list-retry"));
    expect(onLoadMore.mock.calls.length).toBeGreaterThan(1);
  });

  const groups = () =>
    Array.from(screen.getByTestId("search-results").querySelectorAll("h2")).map((h) => h.textContent?.trim());
  const songRows = () => within(screen.getByTestId("search-results")).queryAllByTestId("virtual-track-row");

  it("shows six songs first, then the tile groups, and pages nothing until All songs is chosen", async () => {
    const onLoadMore = vi.fn();
    const onPlayTrack = vi.fn();
    const songs = createSyntheticTracks(9, "s");
    const props = {
      ...base,
      query: "six first",
      songs,
      artists: [{ id: "art-1", name: "Artist" }],
      albums: [{ id: "alb-1", browseId: null, title: "Album" }],
      hasMore: true,
      loadingMore: false,
      moreError: null,
      onLoadMore,
      onPlayTrack,
    };
    render(SearchView, { props });
    const input = screen.getByTestId("search-input") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "six first" } });
    expect(groups()).toEqual(["Songs", "Artists", "Albums"]);
    expect(songRows()).toHaveLength(6);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(onLoadMore).not.toHaveBeenCalled();

    // Rows in the short block play like rows anywhere else.
    await fireEvent.click(songRows()[2]!.querySelector("button")!);
    expect(onPlayTrack).toHaveBeenCalledWith(songs[2]);

    const allSongs = screen.getByTestId("search-all-songs");
    expect(allSongs.textContent?.trim()).toBe("All songs");
    await fireEvent.click(allSongs);
    expect(groups()).toEqual(["Songs"]);
    expect(songRows()).toHaveLength(9);
    await vi.waitFor(() => expect(onLoadMore).toHaveBeenCalled());
    expect(document.activeElement).toBe(screen.getByTestId("search-songs-back"));

    await fireEvent.click(screen.getByTestId("search-songs-back"));
    await vi.waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("search-all-songs")));
    expect(groups()).toEqual(["Songs", "Artists", "Albums"]);
    expect(input.value).toBe("six first");
  });

  it("comes back to the full songs list after the page is left and shown again for the same search", async () => {
    const props = { ...base, query: "round trip", songs: createSyntheticTracks(9, "s"), hasMore: false, loadingMore: false, moreError: null, onLoadMore: () => {} };
    render(SearchView, { props });
    await fireEvent.click(screen.getByTestId("search-all-songs"));
    cleanup();
    const shown = render(SearchView, { props });
    expect(screen.queryByTestId("search-all-songs")).toBeNull();
    expect(songRows()).toHaveLength(9);
    // A new search starts grouped.
    await shown.rerender({ ...props, query: "another" });
    expect(screen.getByTestId("search-all-songs")).toBeTruthy();
    expect(songRows()).toHaveLength(6);
  });

  it("leaves out the Songs block with no songs and All songs with six or fewer", async () => {
    const props = { ...base, query: "abc", songs: [] as Track[], artists: [{ id: "art-1", name: "Artist" }], hasMore: false, loadingMore: false, moreError: null, onLoadMore: () => {} };
    const { rerender } = render(SearchView, { props });
    await fireEvent.input(screen.getByTestId("search-input"), { target: { value: "abc" } });
    expect(groups()).toEqual(["Artists"]);
    expect(screen.queryByTestId("search-all-songs")).toBeNull();
    await rerender({ ...props, songs: createSyntheticTracks(6, "s") });
    expect(groups()).toEqual(["Songs", "Artists"]);
    expect(screen.queryByTestId("search-all-songs")).toBeNull();
  });

  it("searches nothing while an input method is composing, and once when it ends", async () => {
    vi.useFakeTimers();
    try {
      const onSearch = vi.fn();
      render(SearchView, { props: { ...base, query: "", songs: [], hasMore: false, loadingMore: false, moreError: null, onLoadMore: () => {}, onSearch } });
      const input = screen.getByTestId("search-input") as HTMLInputElement;
      await fireEvent.compositionStart(input);
      await fireEvent.input(input, { target: { value: "ni" } });
      await fireEvent.input(input, { target: { value: "nih" } });
      await fireEvent.submit(input.closest("form")!);
      vi.advanceTimersByTime(300);
      expect(onSearch).not.toHaveBeenCalled();
      await fireEvent.compositionEnd(input, { target: { value: "你好" } });
      vi.advanceTimersByTime(249);
      expect(onSearch).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1);
      expect(onSearch).toHaveBeenCalledTimes(1);
      expect(onSearch).toHaveBeenLastCalledWith("你好");
    } finally {
      vi.useRealTimers();
    }
  });

  it("gives Search songs and Home recent plays the like column the other lists have", async () => {
    render(SearchView, {
      props: { ...base, query: "abc", songs: createSyntheticTracks(2, "s"), hasMore: false, loadingMore: false, moreError: null, onLoadMore: () => {}, like },
    });
    expect(document.querySelector(".track-list.has-like")).toBeTruthy();
    cleanup();
    render(HomeView, {
      props: {
        view: ready,
        homeFeed: { recentTracks: createSyntheticTracks(2, "h"), pinnedPlaylists: [] } as unknown as HomeFeed,
        currentVideoId: null,
        mockMode: true,
        like,
        onPlayTrack: () => {},
        onTrackActions: () => {},
        onOpenPlaylist: () => {},
        onOpenRecent: () => {},
        onRetry: () => {},
        onOpenOfficialPlayer: () => {},
      },
    });
    expect(document.querySelector(".track-list.has-like")).toBeTruthy();
  });
});

describe("Page state notes", () => {
  const timeout = { code: "TIMEOUT", retriable: true, retryAfterMs: null, safeMessage: "timed out", requestId: null } as const;

  it("adds no note for content that is only offline or being refetched", () => {
    render(PageState, { props: { view: { kind: "ready", cached: true, error: null }, name: "home", children: (() => {}) as never } });
    expect(screen.queryByTestId("home-state-cached")).toBeNull();
  });

  it("titles an error without its code", () => {
    render(PageState, {
      props: { view: { kind: "error", cached: false, error: { ...timeout, code: "RATE_LIMITED" } }, name: "home", children: (() => {}) as never },
    });
    const box = screen.getByTestId("home-state-error");
    expect(box.textContent).not.toContain("RATE_LIMITED");
    expect(box.getAttribute("title")).toContain("RATE_LIMITED");
  });
});
