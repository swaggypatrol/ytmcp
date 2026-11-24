import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { createSyntheticTracks } from "@ytmcp/protocol";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RETURN_FRAMES_KEY, ReturnFrames, type ListPosition } from "../../return-frame.js";
import VirtualizedTrackList from "../VirtualizedTrackList.svelte";

// The list scrolls with the page's scroll container. jsdom has no layout, so emulate it: the
// list starts `listTop` px into the page, moves up as the page scrolls, and a browser clamps the
// page's scrollTop to its content (everything above the list plus the rows' spacer).
const ROW = 56;
const VIEWPORT = 440;
let listTop = 240;
let main: HTMLElement;
const scrollTopDescriptor = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
const pageScroll = new WeakMap<Element, number>();

beforeEach(() => {
  listTop = 240;
  main = document.body.appendChild(document.createElement("main"));
  main.dataset.testid = "main-scroll-region";
  Object.defineProperty(main, "clientHeight", { value: VIEWPORT, configurable: true });
  Object.defineProperty(Element.prototype, "scrollTop", {
    configurable: true,
    get(this: HTMLElement) {
      return this === main ? (pageScroll.get(this) ?? 0) : scrollTopDescriptor.get!.call(this);
    },
    set(this: HTMLElement, value: number) {
      if (this !== main) return scrollTopDescriptor.set!.call(this, value);
      const rows = parseFloat(main.querySelector<HTMLElement>('[data-testid="virtual-track-list"]')?.style.height ?? "0") || 0;
      const content = listTop + rows;
      pageScroll.set(this, Math.min(Math.max(0, content - VIEWPORT), Math.max(0, value)));
    },
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    const top = this.classList.contains("track-list-body") ? listTop - main.scrollTop : 0;
    return DOMRect.fromRect({ x: 0, y: top, width: 800, height: 0 });
  });
});
afterEach(() => {
  cleanup();
  main.remove();
  vi.restoreAllMocks();
  Object.defineProperty(Element.prototype, "scrollTop", scrollTopDescriptor);
});

const tracks = createSyntheticTracks(200, "rf");
const ids = (prefix: string, count = tracks.length) => Array.from({ length: count }, (_, i) => `${prefix}-${i}`);

function renderList(frames: ReturnFrames, list = tracks, entryIds = ids("e")) {
  return render(VirtualizedTrackList, {
    target: main,
    props: { tracks: list, entryIds, mockMode: true, onPlayTrack: () => {} },
    context: new Map([[RETURN_FRAMES_KEY, frames]]),
  });
}

/** Scrolls the page to row 20 plus 20 px and focuses row 22, as a listener would before leaving. */
async function leaveScrolled(frames: ReturnFrames, pageTop = listTop + 20 * ROW + 20): Promise<ListPosition> {
  const view = renderList(frames);
  main.scrollTop = pageTop;
  await fireEvent.scroll(main);
  const list = screen.getByTestId("virtual-track-list");
  await waitFor(() => expect(list.querySelector('[data-row-index="22"]')).not.toBeNull());
  list.querySelector<HTMLElement>('[data-row-index="22"]')!.focus();
  const position = frames.capture()!;
  view.unmount();
  return position;
}

const focusedRow = () => document.activeElement?.getAttribute("data-row-index");

/** Unmounts the list; cleanup also detaches the render target, so put the page back. */
function unmountAll() {
  cleanup();
  document.body.append(main);
}

describe("Track list return frame", () => {
  it("captures the anchored row and focus, and puts both back when the page returns", async () => {
    const frames = new ReturnFrames();
    const position = await leaveScrolled(frames);
    expect(position).toMatchObject({ anchorKey: "e-20", offsetPx: 20, focusKey: "e-22", focusIndex: 22 });
    expect(frames.capture()).toBeNull();

    const restored = vi.fn();
    frames.offer(position, restored);
    renderList(frames);
    await waitFor(() => expect(restored).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(focusedRow()).toBe("22"));
    expect(main.scrollTop).toBe(listTop + 20 * ROW + 20);
  });

  it("waits for the rows to load before restoring", async () => {
    const frames = new ReturnFrames();
    const position = await leaveScrolled(frames);
    const restored = vi.fn();
    frames.offer(position, restored);
    const view = renderList(frames, [], []);
    await tick();
    expect(restored).not.toHaveBeenCalled();
    await view.rerender({ tracks, entryIds: ids("e") });
    await waitFor(() => expect(restored).toHaveBeenCalledTimes(1));
    expect(main.scrollTop).toBe(listTop + 20 * ROW + 20);
  });

  it("keeps the anchored row in place when the content above the list changed, over the app's raw restore", async () => {
    const frames = new ReturnFrames();
    const position = await leaveScrolled(frames);
    listTop = 400;
    // Like the app: its own restore writes the raw page offset, once directly and once late.
    const raw = () => (main.scrollTop = position.scrollTop);
    frames.offer(position, () => {
      raw();
      queueMicrotask(raw);
    });
    renderList(frames);
    await waitFor(() => expect(focusedRow()).toBe("22"));
    await waitFor(() => expect(main.scrollTop).toBe(400 + 20 * ROW + 20));
  });

  it("brings the header back into view when the page was left above the first row", async () => {
    const frames = new ReturnFrames();
    renderList(frames);
    main.scrollTop = 100;
    await fireEvent.scroll(main);
    const position = frames.capture()!;
    expect(position).toMatchObject({ anchorKey: "e-0", offsetPx: 100 - listTop });
    unmountAll();

    frames.offer(position, () => {});
    renderList(frames);
    await waitFor(() => expect(main.scrollTop).toBe(100));
  });

  it("falls back to the old offset and the nearest row when the anchored rows are gone", async () => {
    const frames = new ReturnFrames();
    const position = await leaveScrolled(frames);

    frames.offer(position, () => {});
    renderList(frames, tracks, ids("other"));
    await waitFor(() => expect(focusedRow()).toBe("22"));
    expect(main.scrollTop).toBe(listTop + 20 * ROW + 20);
    unmountAll();

    // A shorter list: the page clamps the offset to its end and the last row takes focus.
    frames.offer(position, () => {});
    renderList(frames, tracks.slice(0, 10), ids("other", 10));
    await waitFor(() => expect(focusedRow()).toBe("9"));
    expect(main.scrollTop).toBe(listTop + 10 * ROW - VIEWPORT);
  });

  it("leaves focus alone when the list did not have it", async () => {
    const frames = new ReturnFrames();
    renderList(frames);
    main.scrollTop = listTop + 580;
    await fireEvent.scroll(main);
    const position = frames.capture()!;
    expect(position.focusKey).toBeNull();
    unmountAll();

    const outside = document.createElement("button");
    document.body.append(outside);
    outside.focus();
    const restored = vi.fn();
    frames.offer(position, restored);
    renderList(frames);
    await waitFor(() => expect(restored).toHaveBeenCalled());
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });
});
