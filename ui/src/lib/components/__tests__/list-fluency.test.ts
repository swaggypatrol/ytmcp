import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { FakeCoreClient, createSyntheticTracks, type AppSnapshot, type QueueEntry, type QueueSummary, type Track } from "@ytmcp/protocol";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PageView } from "../../catalog/page-state.js";
import QueueView from "../QueueView.svelte";
import TrackCollectionView from "../TrackCollectionView.svelte";
import VirtualizedTrackList from "../VirtualizedTrackList.svelte";

// Lists render inside the app's one scroll container; jsdom has no layout, so the page is
// 440 px tall and the list starts at the top of its content.
const VIEWPORT = 440;
const ROW = 56;
let main: HTMLElement;
beforeEach(() => {
  main = document.body.appendChild(document.createElement("main"));
  main.dataset.testid = "main-scroll-region";
  Object.defineProperty(main, "clientHeight", { value: VIEWPORT, configurable: true });
});
afterEach(() => {
  cleanup();
  main.remove();
});

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

const rowButton = (scope: ParentNode, key: string) =>
  scope.querySelector<HTMLElement>(`[data-row-key="${key}"] [data-row-index]`);
const focusedKey = () => document.activeElement?.closest("[data-row-key]")?.getAttribute("data-row-key") ?? null;
/** Keys of the rows whose main button is the list's tab stop. */
const tabStops = (scope: ParentNode) =>
  [...scope.querySelectorAll<HTMLElement>('[data-row-index][tabindex="0"]')].map(
    (b) => b.closest("[data-row-key]")?.getAttribute("data-row-key")
  );

describe("Keyboard focus follows its row when the rows change", () => {
  const tracks = available(20, "fk");
  const ids = tracks.map((_, i) => `k-${i}`);

  it("keeps the tab stop on the focused row when a row above it goes, so the next arrow moves one row", async () => {
    const view = renderList({ tracks, entryIds: ids });
    const list = screen.getByTestId("virtual-track-list");
    rowButton(list, "k-5")!.focus();
    await tick();

    await view.rerender({ tracks: tracks.slice(1), entryIds: ids.slice(1) });
    await tick();
    expect(focusedKey()).toBe("k-5");
    expect(tabStops(list)).toEqual(["k-5"]);

    await fireEvent.keyDown(list, { key: "ArrowDown" });
    await waitFor(() => expect(focusedKey()).toBe("k-6"));
  });

  it("puts the tab stop on the row now in its place when the focused row is gone, without taking focus", async () => {
    const view = renderList({ tracks, entryIds: ids });
    const list = screen.getByTestId("virtual-track-list");
    rowButton(list, "k-5")!.focus();
    await tick();

    await view.rerender({ tracks: tracks.filter((_, i) => i !== 5), entryIds: ids.filter((_, i) => i !== 5) });
    await tick();
    expect(list.contains(document.activeElement)).toBe(false);
    expect(tabStops(list)).toEqual(["k-6"]);
  });

  it("falls back from where the row last was, not where it was focused", async () => {
    const view = renderList({ tracks, entryIds: ids });
    const list = screen.getByTestId("virtual-track-list");
    rowButton(list, "k-5")!.focus();
    await tick();
    // Two rows above go (k-5 moves to index 3), then k-5 itself.
    await view.rerender({ tracks: tracks.slice(2), entryIds: ids.slice(2) });
    await tick();
    expect(tabStops(list)).toEqual(["k-5"]);
    await view.rerender({ tracks: tracks.slice(2).filter((_, i) => i !== 3), entryIds: ids.slice(2).filter((_, i) => i !== 3) });
    await tick();
    expect(tabStops(list)).toEqual(["k-6"]);
  });

  it("re-keys the rows when entry ids change at the same count (a queue move)", async () => {
    const three = tracks.slice(0, 3);
    const view = renderList({ tracks: three, entryIds: ["a", "b", "c"] });
    const keys = () =>
      [...screen.getByTestId("virtual-track-list").querySelectorAll<HTMLElement>("[data-row-index]")]
        .sort((x, y) => Number(x.dataset.rowIndex) - Number(y.dataset.rowIndex))
        .map((b) => b.closest("[data-row-key]")?.getAttribute("data-row-key"));
    expect(keys()).toEqual(["a", "b", "c"]);
    await view.rerender({ tracks: [three[1]!, three[0]!, three[2]!], entryIds: ["b", "a", "c"] });
    await tick();
    expect(keys()).toEqual(["b", "a", "c"]);
  });

  it("does not rebuild the rows' measurements on an arrow press", async () => {
    const big = available(10_000, "big");
    let reads = 0;
    const entryIds = new Proxy(
      big.map((_, i) => `b-${i}`),
      {
        get(target, prop, receiver) {
          if (typeof prop === "string" && /^\d+$/.test(prop)) reads++;
          return Reflect.get(target, prop, receiver);
        },
      }
    );
    renderList({ tracks: big, entryIds });
    const list = screen.getByTestId("virtual-track-list");
    rowButton(list, "b-0")!.focus();
    await tick();

    reads = 0;
    await fireEvent.keyDown(list, { key: "ArrowDown" });
    await waitFor(() => expect(focusedKey()).toBe("b-1"));
    await tick();
    // A rebuild reads the key of every one of the 10,000 rows; a focus move reads only a few rendered ones.
    expect(reads).toBeLessThan(500);
  });
});

describe("Queue page focus across a track change", () => {
  const base: AppSnapshot = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
  const entries: QueueEntry[] = createSyntheticTracks(10, "e").map((track, i) => ({
    entryId: `e-${i}`,
    videoId: track.videoId,
    track: { ...track, availability: "available" },
    availability: "available",
  }));
  const summary = (current: number): QueueSummary => ({
    queueRevision: 1,
    currentEntryId: entries[current]!.entryId,
    completeness: "complete",
    hasMore: false,
    loadedStart: 0,
    loadedCount: entries.length,
    previewStart: 0,
    previewEntries: entries,
  });
  const player = (current: number) => ({
    ...base.player,
    connection: "ready" as const,
    currentTrack: entries[current]!.track,
    currentQueueEntryId: entries[current]!.entryId,
    trackInstanceId: "ti-1",
  });

  it("keeps an Up next row focused and its tab stop when the current entry advances, and ArrowDown moves one row", async () => {
    const view = render(QueueView, {
      target: main,
      props: {
        queueSummary: summary(2),
        capabilities: base.capabilities,
        connection: "ready",
        player: player(2),
        mockMode: true,
        onSelectEntry: () => {},
        onTrackActions: () => {},
      },
    });
    const upNext = screen.getByTestId("queue-up-next");
    rowButton(upNext, "e-5")!.focus();
    await tick();

    await view.rerender({ queueSummary: summary(3), player: player(3) });
    await tick();
    expect(focusedKey()).toBe("e-5");
    expect(tabStops(upNext)).toEqual(["e-5"]);

    await fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    await waitFor(() => expect(focusedKey()).toBe("e-6"));
  });
});

describe("No late height changes at the list's end", () => {
  const READY: PageView = { kind: "ready", cached: false, error: null };

  it("reserves the song count line before the last page lands", async () => {
    const view = render(TrackCollectionView, {
      target: main,
      props: {
        name: "liked",
        title: "Liked songs",
        icon: "heart",
        view: READY,
        tracks: available(3, "lc"),
        hasMore: true,
        loadingMore: false,
        moreError: null,
        currentVideoId: null,
        mockMode: true,
        onRetry: () => {},
        onOpenOfficialPlayer: () => {},
        onLoadMore: () => {},
        onPlayTrack: () => {},
        onTrackActions: () => {},
      },
    });
    const line = screen.getByTestId("liked-count");
    expect(line.textContent?.trim()).toBe("");
    await view.rerender({ hasMore: false });
    expect(screen.getByTestId("liked-count")).toBe(line);
    expect(line.textContent?.trim()).toBe("3 songs");

    // jsdom has no layout: the empty line keeps one line of height through CSS.
    const css = readFileSync(resolve(import.meta.dirname, "../../../styles/lists.css"), "utf8");
    expect(css).toMatch(/\.collection-count\s*\{[^}]*min-height:\s*1lh/);
  });

  it("keeps the tail's space after an empty last page until the end is scrolled out of view", async () => {
    const tracks = available(200, "tl");
    const view = renderList({ tracks, hasMore: true, onLoadMore: () => {} });
    // The page's bottom: 200 rows and the tail end at 201 rows.
    await scrollPage(201 * ROW - VIEWPORT);

    await view.rerender({ hasMore: false });
    await tick();
    const tail = screen.getByTestId("track-list-tail");
    expect(tail.style.height).toBe(`${ROW}px`);
    expect(tail.getAttribute("aria-hidden")).toBe("true");
    expect(tail.querySelector('[role="status"]')).toBeNull();

    // Scrolled a screen up, the space below the window can go without moving anything in view.
    await scrollPage(150 * ROW);
    await waitFor(() => expect(screen.queryByTestId("track-list-tail")).toBeNull());
  });

  it("drops the tail of a list that ends inside the window when removing it cannot pull the page up", async () => {
    // Five rows end at 280 px, inside the 440 px window; content after the list makes the page 1,000 px.
    Object.defineProperty(main, "scrollHeight", { value: 1000, configurable: true });
    const view = renderList({ tracks: available(5, "ts"), hasMore: true, onLoadMore: () => {} });
    await tick();
    await view.rerender({ hasMore: false });
    await tick();
    expect(screen.queryByTestId("track-list-tail")).toBeNull();
  });

  it("drops the tail at once when the last page lands while its end is out of view", async () => {
    const view = renderList({ tracks: available(200, "tf"), hasMore: true, onLoadMore: () => {} });
    await scrollPage(50 * ROW);
    await view.rerender({ hasMore: false });
    await tick();
    expect(screen.queryByTestId("track-list-tail")).toBeNull();
  });
});

describe("Reduced motion keeps the short fades it means to keep", () => {
  // The global reset (motion-a11y.css) sets transition-duration with !important, so a fade
  // that survives it must be !important too.
  const css = (file: string) => readFileSync(resolve(import.meta.dirname, `../../../styles/${file}`), "utf8");

  it("fades the collection strip, without moving it", () => {
    const rules = [...css("lists.css").matchAll(/\.collection-strip\.is-shown\s*\{([^}]*)\}/g)].map((m) => m[1]!);
    // The reduced-motion rules are the ones whose transition leaves transform out.
    const reduced = rules.filter((body) => !/transform var\(/.test(body));
    expect(reduced).toHaveLength(2);
    for (const body of reduced) {
      expect(body).toMatch(/transition:\s*opacity var\(--duration-fast\) ease-out !important;/);
    }
  });

  it("fades the buffer ring and the scrub flow", () => {
    const rules = [...css("player-bar.css").matchAll(/\.player-buffer-ring,\s*(?::root\[data-reduce-motion="true"\]\s*)?\.scrub-flow\s*\{([^}]*)\}/g)].map((m) => m[1]!);
    expect(rules).toHaveLength(2);
    for (const body of rules) expect(body).toMatch(/transition-duration:\s*var\(--duration-fast\) !important;/);
  });
});
