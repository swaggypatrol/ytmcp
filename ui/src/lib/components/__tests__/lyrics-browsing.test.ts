import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { createSyntheticTracks, type LyricsResult } from "@ytmcp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import LyricsView from "../LyricsView.svelte";

const originalScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  if (originalScrollTo) Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
});

const [trackA, trackB] = createSyntheticTracks(2, "ly");

const LYRICS: LyricsResult = {
  videoId: trackA!.videoId,
  kind: "synced",
  source: "lrclib",
  language: null,
  matchBasis: null,
  fetchedAtMs: 0,
  fileOffsetMs: 0,
  userOffsetMs: 0,
  syncedLines: Array.from({ length: 30 }, (_, i) => ({ timeMs: i * 1000, text: `Line ${i}` })),
  plainText: null,
  error: null,
};

function renderLyrics(smoothScroll = true, followActive = true) {
  const scrollTo = vi.fn();
  // jsdom has no layout: give the list a scroll method and a height it can overflow.
  Object.defineProperty(HTMLElement.prototype, "scrollTo", { value: scrollTo, configurable: true, writable: true });
  const props = {
    track: trackA!,
    lyrics: LYRICS,
    loading: false,
    interpolatedPositionMs: 500,
    canSeek: true,
    mockMode: true,
    followActive,
    smoothScroll,
    accent: null,
    onSeekMs: vi.fn(),
    onStepOffset: () => {},
    onResetOffset: () => {},
  };
  const view = render(LyricsView, { props });
  const list = screen.getByTestId("lyrics-synced-list");
  Object.defineProperty(list, "scrollHeight", { value: 2000, configurable: true });
  Object.defineProperty(list, "clientHeight", { value: 300, configurable: true });
  const at = (ms: number) => view.rerender({ ...props, interpolatedPositionMs: ms });
  return { ...view, props, list, scrollTo, at };
}

const backButton = () => screen.queryByTestId("lyrics-back-to-current");
const activeLine = (list: HTMLElement) => list.querySelector('[aria-current="true"]')?.getAttribute("data-line-index");

describe("Lyrics manual browsing", () => {
  it("pauses following on a hand scroll, keeps highlighting, and resumes only when asked", async () => {
    const { list, scrollTo, at } = renderLyrics();
    await at(1500);
    expect(scrollTo).toHaveBeenCalled();

    // Scroll events alone (the follow scroll emits them too) never count as browsing.
    await fireEvent.scroll(list);
    expect(backButton()).toBeNull();

    await fireEvent.wheel(list, { deltaY: 120 });
    expect(backButton()).not.toBeNull();
    scrollTo.mockClear();
    await at(2500);
    await at(3500);
    // The next lines highlight but do not pull the list back.
    expect(activeLine(list)).toBe("3");
    expect(scrollTo).not.toHaveBeenCalled();
    expect(backButton()).not.toBeNull();

    await fireEvent.click(backButton()!);
    expect(backButton()).toBeNull();
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(scrollTo.mock.lastCall![0]).toMatchObject({ behavior: "smooth" });
    await at(4500);
    expect(scrollTo).toHaveBeenCalledTimes(2);
  });

  it("treats touch drags and scroll keys as browsing, and follows again on a new track", async () => {
    const { list, scrollTo, rerender, props } = renderLyrics();
    await fireEvent.touchMove(list);
    expect(backButton()).not.toBeNull();
    await fireEvent.click(backButton()!);

    await fireEvent.keyDown(list.querySelector('[data-line-index="0"]')!, { key: "PageDown" });
    expect(backButton()).not.toBeNull();
    scrollTo.mockClear();
    await rerender({ ...props, track: trackB!, lyrics: { ...LYRICS, videoId: trackB!.videoId }, interpolatedPositionMs: 2500 });
    expect(backButton()).toBeNull();
    expect(scrollTo).toHaveBeenCalled();
  });

  it("returns to the current line while paused, when the follow effect stays still", async () => {
    const { list, scrollTo, at } = renderLyrics(false, false);
    await at(4500);
    await fireEvent.wheel(list, { deltaY: 120 });
    scrollTo.mockClear();
    await fireEvent.click(backButton()!);
    expect(backButton()).toBeNull();
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(scrollTo.mock.lastCall![0]).toMatchObject({ behavior: "auto" });
  });

  it("moves focus from the back button to the current line, or to the list when lines cannot seek", async () => {
    const { list, at, rerender, props } = renderLyrics();
    await at(3500);
    await fireEvent.keyDown(list.querySelector('[data-line-index="0"]')!, { key: "PageDown" });
    backButton()!.focus();
    await fireEvent.click(backButton()!);
    expect(document.activeElement).toBe(list.querySelector('[data-line-index="3"]'));

    await rerender({ ...props, interpolatedPositionMs: 3500, canSeek: false });
    await fireEvent.wheel(list, { deltaY: 120 });
    backButton()!.focus();
    await fireEvent.click(backButton()!);
    expect(document.activeElement).toBe(list);
  });

  it("jumps without smooth scrolling under reduced motion and still marks the current line", async () => {
    const { list, scrollTo, at } = renderLyrics(false);
    await at(2500);
    expect(scrollTo.mock.lastCall![0]).toMatchObject({ behavior: "auto" });
    expect(activeLine(list)).toBe("2");
  });
});
