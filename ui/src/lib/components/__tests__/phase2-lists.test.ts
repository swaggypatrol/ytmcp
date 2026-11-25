import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { FakeCoreClient, createSyntheticTracks, type QueueEntry, type Track } from "@ytmcp/protocol";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "../../../app/App.svelte";
import { createUiClient } from "../../ipc/client.js";
import type { PageView } from "../../catalog/page-state.js";
import QueueView from "../QueueView.svelte";
import TrackCollectionView from "../TrackCollectionView.svelte";
import VirtualizedTrackList from "../VirtualizedTrackList.svelte";

// Lists render inside the app's one scroll container; jsdom has no layout, so the page is
// 440 px tall and the list starts at the top of its content.
const VIEWPORT = 440;
let main: HTMLElement;
beforeEach(() => {
  main = document.body.appendChild(document.createElement("main"));
  main.dataset.testid = "main-scroll-region";
  Object.defineProperty(main, "clientHeight", { value: VIEWPORT, configurable: true });
});
afterEach(() => {
  cleanup();
  main.remove();
  vi.restoreAllMocks();
});

const READY: PageView = { kind: "ready", cached: false, error: null };
const available = (count: number, prefix: string): Track[] =>
  createSyntheticTracks(count, prefix).map((track) => ({ ...track, availability: "available" }));

function renderList(props: Record<string, unknown>) {
  return render(VirtualizedTrackList, { target: main, props: { tracks: [] as Track[], mockMode: true, onPlayTrack: () => {}, ...props } });
}

async function scrollPage(top: number) {
  main.scrollTop = top;
  await fireEvent.scroll(main);
  await tick();
}

describe("Track list on the page scroller", () => {
  it("has no height or scroll box of its own and keeps the DOM bounded for 10,000 rows", async () => {
    renderList({ tracks: available(10_000, "big") });
    const body = screen.getByTestId("virtual-track-list");
    expect(body.style.height).toBe(`${10_000 * 56}px`);
    expect(body.style.overflow + body.style.overflowY).toBe("");
    expect(body.className).not.toMatch(/overflow/);

    await scrollPage(5000 * 56);
    await waitFor(() => expect(body.querySelector('[data-row-index="5000"]')).not.toBeNull());
    const rows = screen.getAllByTestId("virtual-track-row");
    expect(rows.length).toBeLessThan(40);
    // Rows are placed with transforms only.
    expect(rows.every((row) => /translateY\(/.test(row.style.transform) && row.style.top === "")).toBe(true);
  });

  it("asks for the next page within 1.5 screens of the end, never while loading or after a failure, and retries inline", async () => {
    const onLoadMore = vi.fn();
    const view = renderList({ tracks: available(200, "pg"), hasMore: true, onLoadMore });
    await tick();
    expect(onLoadMore).not.toHaveBeenCalled();
    expect(screen.queryByText(/load more/i)).toBeNull();

    // 200 rows end at 11,200 px: from 10,000 the end is 1,200 - 440 = 760 px (< 660 is the trigger).
    await scrollPage(10_000);
    expect(onLoadMore).not.toHaveBeenCalled();
    await scrollPage(10_300);
    await waitFor(() => expect(onLoadMore).toHaveBeenCalledTimes(1));

    await view.rerender({ loadingMore: true });
    await scrollPage(10_400);
    expect(onLoadMore).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("track-list-tail").textContent).toContain("Loading more songs");

    await view.rerender({ loadingMore: false, moreFailed: true });
    await scrollPage(10_500);
    expect(onLoadMore).toHaveBeenCalledTimes(1);
    const tail = screen.getByTestId("track-list-tail");
    expect(within(tail).getByRole("status").textContent).toContain("Couldn’t load more songs");
    await fireEvent.click(within(tail).getByTestId("track-list-retry"));
    expect(onLoadMore).toHaveBeenCalledTimes(2);
  });

  it("fetches again right away when a short first page does not fill the window", async () => {
    const onLoadMore = vi.fn();
    renderList({ tracks: available(3, "short"), hasMore: true, onLoadMore });
    await waitFor(() => expect(onLoadMore).toHaveBeenCalledTimes(1));
  });

  it("shows a like toggle only when the list has one, pressed for liked songs", async () => {
    const tracks = available(3, "lk");
    const toggle = vi.fn();
    const view = renderList({
      tracks,
      like: { liked: (t: Track) => t.videoId === tracks[1]!.videoId, pending: () => false, toggle },
    });
    const buttons = screen.getAllByTestId("track-like-btn");
    expect(buttons.map((b) => b.getAttribute("aria-pressed"))).toEqual(["false", "true", "false"]);
    expect(buttons[1]!.getAttribute("aria-label")).toBe(`Remove like from ${tracks[1]!.title}`);
    await fireEvent.click(buttons[0]!);
    expect(toggle).toHaveBeenCalledWith(tracks[0]);
    view.unmount();

    renderList({ tracks });
    expect(screen.queryAllByTestId("track-like-btn")).toHaveLength(0);
  });

  it("leaves the like cell empty when a song's like status is unknown", () => {
    const tracks = available(2, "unk");
    renderList({
      tracks,
      like: { liked: (t: Track) => (t.videoId === tracks[0]!.videoId ? true : null), pending: () => false, toggle: vi.fn() },
    });
    const rows = screen.getAllByTestId("virtual-track-row");
    expect(within(rows[0]!).getByTestId("track-like-btn").getAttribute("aria-pressed")).toBe("true");
    expect(within(rows[1]!).queryByTestId("track-like-btn")).toBeNull();
    expect(rows[1]!.querySelector(".tl-like")).not.toBeNull();
  });

  it("uses the same grid tracks and cell order for the header and every row", () => {
    const tracks = available(2, "hd");
    renderList({
      tracks, showHeader: true, onTrackActions: () => {}, rowActionCount: 3,
      like: { liked: (track: Track) => track.isLiked ?? null, pending: () => false, toggle: () => {} },
    });
    const header = document.querySelector<HTMLElement>(".track-list-header")!;
    const row = screen.getAllByTestId("virtual-track-row")[0]!;
    const cells = (el: HTMLElement) => [...el.children].map((child) => child.className);
    expect(cells(header)).toEqual(["tl-main", "tl-actions", "tl-like", "tl-dur", "tl-more"]);
    expect(cells(row)).toEqual(cells(header));
    expect(header.querySelector(".tl-title-head")?.textContent).toBe("Title");
    expect(row.querySelector(".tl-dur")?.textContent).toBe("3:05");
    expect(within(row).getByTestId("track-like-btn").getAttribute("aria-pressed")).toBe("true");
    expect(within(row).getByTestId("track-actions-btn")).toBeTruthy();
    expect(row.closest<HTMLElement>(".track-list")?.style.getPropertyValue("--tl-action-count")).toBe("3");

    const css = readFileSync(resolve(import.meta.dirname, "../../../styles/lists.css"), "utf8");
    expect(css).toMatch(/\.track-list-header,\s*\.tl-row\s*\{[^}]*display:\s*grid;[^}]*grid-template-columns:\s*var\(--tl-columns\);[^}]*column-gap:\s*var\(--space-16\);[^}]*padding-inline:\s*var\(--space-12\)/);
    expect(css).toMatch(/\.tl-main\s*\{[^}]*grid-template-columns:\s*subgrid/);
  });

  it("introduces like at Regular and keeps every action available at wider tiers", () => {
    // jsdom cannot evaluate the content-width queries.
    const css = readFileSync(resolve(import.meta.dirname, "../../../styles/lists.css"), "utf8");
    const tiers = new Map([...css.matchAll(
      /@container content \(min-width:\s*(\d+)px\)\s*\{((?:[^{}]|\{[^{}]*\})*)\}/g
    )].map(([, width, rules]) => [Number(width), rules!]));
    const regular = tiers.get(880);
    expect(regular).toMatch(/\.track-list\.has-like\s*\{[^}]*--tl-like-column:\s*40px/);
    expect(regular).toMatch(/\.tl-like\s*\{[^}]*display:\s*grid/);
    for (const [width, rules] of tiers) {
      if (width < 1400) continue;
      expect(rules).not.toMatch(/\.(?:tl-like|tl-more|tl-actions)\b[^{]*\{[^}]*display\s*:/);
      expect(rules).not.toMatch(/--tl-(?:like|more|action)-column\s*:/);
    }
  });

  it("toggles a known song through the normal mock UI and refreshes Liked songs", async () => {
    const client = createUiClient({ mockScenario: "normal" });
    const song = createSyntheticTracks(2)[1]!;
    render(App, { props: { client } });
    const home = within(await screen.findByTestId("view-home"));
    const heart = await home.findByRole("button", { name: `Like ${song.title}` });
    expect(heart.getAttribute("aria-pressed")).toBe("false");
    await fireEvent.click(heart);
    await waitFor(() => expect(heart.getAttribute("aria-pressed")).toBe("true"));
    expect(screen.getByTestId("toast-region").querySelector(".toast")).toBeNull();

    await fireEvent.click(screen.getByTestId("nav-liked"));
    const liked = within(await screen.findByTestId("view-liked"));
    const inCollection = await liked.findByRole("button", { name: `Remove like from ${song.title}` });
    await fireEvent.click(inCollection);
    // The unliked row stays where it is, its heart empty: the list is not rebuilt under the
    // listener. It is read again the next time the page opens, and the song is gone then.
    await waitFor(() => expect(liked.getByRole("button", { name: `Like ${song.title}` }).getAttribute("aria-pressed")).toBe("false"));
    await new Promise((r) => setTimeout(r, 20));
    expect(liked.getByText(song.title)).toBeTruthy();
    await fireEvent.click(screen.getByTestId("nav-home"));
    await screen.findByTestId("view-home");
    await fireEvent.click(screen.getByTestId("nav-liked"));
    const reopened = within(await screen.findByTestId("view-liked"));
    await waitFor(() => expect(reopened.queryByText(song.title)).toBeNull());
  });

  it("marks the current row with the playback indicator and a Rose title, keeping the number while idle", async () => {
    const tracks = available(3, "cur");
    const view = renderList({
      tracks,
      currentVideoId: tracks[1]!.videoId,
      nowPlaying: { playback: "playing", connection: "ready", motion: true },
    });
    const rows = screen.getAllByTestId("virtual-track-row");
    expect(rows[1]!.classList.contains("is-current")).toBe(true);
    expect(within(rows[1]!).getByTestId("playback-indicator").getAttribute("data-state")).toBe("playing");
    expect(within(rows[0]!).queryByTestId("playback-indicator")).toBeNull();

    await view.rerender({ nowPlaying: { playback: "idle", connection: "ready", motion: true } });
    expect(screen.queryByTestId("playback-indicator")).toBeNull();
    expect(screen.getAllByTestId("virtual-track-row")[1]!.querySelector(".tl-num")!.textContent?.trim()).toBe("2");
  });
});

describe("Collection header", () => {
  function renderCollection(props: Record<string, unknown>) {
    return render(TrackCollectionView, {
      target: main,
      props: {
        name: "playlist",
        title: "Night drive",
        view: READY,
        tracks: available(3, "col"),
        hasMore: false,
        loadingMore: false,
        moreError: null,
        currentVideoId: null,
        mockMode: true,
        onRetry: () => {},
        onOpenOfficialPlayer: () => {},
        onLoadMore: () => {},
        onPlayTrack: () => {},
        onTrackActions: () => {},
        ...props,
      },
    });
  }

  it("without a known total, counts only once every page is loaded, never a loaded counter or Load more", async () => {
    const view = renderCollection({ hasMore: true, subtitle: "SyntheticUser" });
    expect(screen.getByTestId("playlist-count").textContent?.trim()).toBe("SyntheticUser");
    expect(screen.getByTestId("view-playlist").textContent).not.toMatch(/Loaded|Load more/);
    await view.rerender({ hasMore: false, tracks: available(400, "col") });
    expect(screen.getByTestId("playlist-count").textContent?.trim()).toBe("SyntheticUser · 400 songs");
  });

  it("shows a known total while pages remain", () => {
    renderCollection({ hasMore: true, total: 10_000 });
    expect(screen.getByTestId("playlist-count").textContent?.trim()).toBe(`${(10_000).toLocaleString()} songs`);
  });

  it("offers Play only when the collection plays as a whole, starting at its first playable song", async () => {
    const tracks = available(3, "pl");
    tracks[0] = { ...tracks[0]!, availability: "unavailable" };
    const onPlayTrack = vi.fn();
    const view = renderCollection({ tracks, onPlayTrack });
    expect(screen.queryByTestId("playlist-play")).toBeNull();
    expect(screen.queryByText("Shuffle")).toBeNull();

    await view.rerender({ playAll: true });
    await fireEvent.click(screen.getByTestId("playlist-play"));
    expect(onPlayTrack).toHaveBeenCalledWith(tracks[1], 1);
  });

  it("plays a collection with its own context from the header and the strip, Shuffle only when it can work", async () => {
    const onPlay = vi.fn();
    const onShuffle = vi.fn();
    const onPlayTrack = vi.fn();
    const collection = { pending: null, canShuffle: true, onPlay, onShuffle };
    const view = renderCollection({ name: "liked", collection, onPlayTrack });
    await fireEvent.click(screen.getByTestId("liked-play"));
    await fireEvent.click(screen.getByTestId("liked-strip-play"));
    await fireEvent.click(screen.getByTestId("liked-shuffle"));
    expect(onPlay).toHaveBeenCalledTimes(2);
    expect(onShuffle).toHaveBeenCalledTimes(1);
    expect(onPlayTrack).not.toHaveBeenCalled();

    await view.rerender({ collection: { ...collection, pending: "shuffle" } });
    expect(screen.getByTestId("liked-shuffle").getAttribute("data-pending")).toBe("true");
    expect(screen.getByTestId("liked-shuffle").getAttribute("aria-busy")).toBe("true");
    expect(screen.getByTestId("liked-play").getAttribute("data-pending")).toBeNull();

    await view.rerender({ collection: { ...collection, canShuffle: false } });
    expect(screen.getByTestId("liked-play")).toBeTruthy();
    expect(screen.queryByTestId("liked-shuffle")).toBeNull();

    await view.rerender({ collection: null });
    expect(screen.queryByTestId("liked-play")).toBeNull();
    expect(screen.queryByTestId("liked-strip-play")).toBeNull();
  });

  it("condenses into the strip once the title scrolls behind it, without taking any space", async () => {
    let titleBottom = 200;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const title = this.classList.contains("collection-title");
      return DOMRect.fromRect({ x: 0, y: title ? titleBottom - 40 : 0, width: 800, height: title ? 40 : 0 });
    });
    renderCollection({ playAll: true });
    const strip = screen.getByTestId("playlist-strip");
    expect(strip.classList.contains("is-shown")).toBe(false);
    // Svelte sets `inert` as a property; jsdom does not reflect it to the attribute.
    const inert = () => strip.hasAttribute("inert") || (strip as HTMLElement & { inert?: boolean }).inert === true;
    expect(inert()).toBe(true);
    expect(strip.getAttribute("aria-hidden")).toBe("true");
    expect(strip.parentElement!.classList.contains("collection-strip-anchor")).toBe(true);

    titleBottom = 30;
    await fireEvent.scroll(main);
    await waitFor(() => expect(strip.classList.contains("is-shown")).toBe(true));
    expect(inert()).toBe(false);
    expect(within(strip).getByTestId("playlist-strip-play").getAttribute("aria-label")).toBe("Play Night drive");

    titleBottom = 120;
    await fireEvent.scroll(main);
    await waitFor(() => expect(strip.classList.contains("is-shown")).toBe(false));
  });
});

describe("Queue page rows", () => {
  function entries(count: number): QueueEntry[] {
    return available(count, "q").map((track, i) => ({ entryId: `q-${i}`, videoId: track.videoId, track, availability: "available" }));
  }

  it("draws every section with page-scrolled rows and no internal state labels", () => {
    const base = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
    const preview = entries(120);
    render(QueueView, {
      target: main,
      props: {
        queueSummary: {
          queueRevision: 7,
          currentEntryId: "q-0",
          completeness: "partial",
          hasMore: true,
          loadedCount: 120,
          previewEntries: preview,
        },
        capabilities: base.capabilities,
        connection: "ready",
        player: { ...base.player, connection: "ready", currentQueueEntryId: "q-0", trackInstanceId: "ti" },
        mockMode: true,
        onSelectEntry: () => {},
        onTrackActions: () => {},
      },
    });
    const upNext = within(screen.getByTestId("queue-up-next")).getByTestId("virtual-track-list");
    expect(upNext.style.height).toBe(`${119 * 56}px`);
    const text = screen.getByTestId("view-queue").textContent ?? "";
    expect(text).not.toMatch(/revision|Partial queue|More items follow|appears more than once/i);
  });

  it("keeps room for queue edit controls in the flat header and in rows that cannot be edited", async () => {
    const base = new FakeCoreClient().subscribeSnapshot(() => {}).snapshot;
    const queueSummary = {
      queueRevision: 1,
      currentEntryId: "q-1",
      completeness: "complete" as const,
      hasMore: false,
      loadedCount: 4,
      previewEntries: entries(4),
    };
    const view = render(QueueView, {
      target: main,
      props: {
        queueSummary,
        capabilities: { ...base.capabilities, queueRemove: "supported", queueReorder: "supported" },
        connection: "ready",
        player: { ...base.player, currentQueueEntryId: "q-1" },
        mockMode: true,
        onSelectEntry: () => {},
        onTrackActions: () => {},
      },
    });
    const now = screen.getByTestId("queue-now");
    const next = screen.getByTestId("queue-up-next");
    for (const section of [now, next]) {
      expect(section.querySelector<HTMLElement>(".track-list")?.style.getPropertyValue("--tl-action-count")).toBe("3");
    }
    expect(now.querySelector(".tl-actions")?.children).toHaveLength(0);
    expect(within(next).getAllByTestId("queue-row-actions")[0]?.parentElement?.className).toBe("tl-actions");

    await view.rerender({ queueSummary: { ...queueSummary, currentEntryId: null } });
    const header = screen.getByTestId("view-queue").querySelector(".track-list-header")!;
    const row = screen.getAllByTestId("virtual-track-row")[0]!;
    expect(header.querySelector(".tl-actions")).not.toBeNull();
    expect(row.querySelector(".tl-actions")).not.toBeNull();
    expect(row.closest<HTMLElement>(".track-list")?.style.getPropertyValue("--tl-action-count")).toBe("3");
  });

  it("keeps Rose off the queue header: the position is plain text and the autoplay switch a 32 px chip", () => {
    const base = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
    const preview = entries(4);
    render(QueueView, {
      target: main,
      props: {
        queueSummary: { queueRevision: 1, currentEntryId: "q-1", completeness: "complete", hasMore: false, loadedStart: 0, loadedCount: 4, previewStart: 0, previewEntries: preview },
        capabilities: { ...base.capabilities, queueAutoplay: "supported" },
        connection: "ready",
        player: { ...base.player, connection: "ready", currentQueueEntryId: "q-1", trackInstanceId: "ti", autoplay: true },
        mockMode: true,
        onSelectEntry: () => {},
        onTrackActions: () => {},
        onCommand: vi.fn(async () => null),
      },
    });
    const position = screen.getByTestId("queue-position");
    expect(position.textContent?.trim()).toBe("Track 2 of 4");
    expect(position.classList.contains("chip")).toBe(false);
    const autoplay = screen.getByTestId("queue-autoplay-toggle");
    expect(autoplay.getAttribute("aria-checked")).toBe("true");
    expect(autoplay.textContent?.trim()).toBe("Autoplay");
    expect(autoplay.classList.contains("queue-chip")).toBe(true);
    expect(screen.getByTestId("view-queue").querySelector(".chip-accent")).toBeNull();

    const css = readFileSync(resolve(import.meta.dirname, "../../../styles/lists.css"), "utf8");
    expect(css).toMatch(/\.queue-chip \{[^}]*min-height: 32px/);
    // A waiting filter's dot is out of flow, so the chip does not grow.
    expect(css).toMatch(/\.queue-chip > \.wait-dot \{[^}]*position: absolute/);
  });
});
