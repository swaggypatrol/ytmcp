<script lang="ts" module>
  /** The part of a wheel event the shelf reads. */
  export type ShelfWheel = Pick<WheelEvent, "deltaX" | "deltaY" | "deltaMode" | "shiftKey" | "ctrlKey">;

  const LINE_PX = 40;

  /**
   * Where a wheel turn moves the shelf, or null when the shelf leaves it alone: sideways
   * gestures (trackpads, a Shift+wheel the browser already turned) scroll natively, Ctrl
   * zooms, and a turn the shelf cannot follow any further goes on to the page. With Shift the
   * shelf keeps the turn even at an end. `from` and `max` are scroll offsets in px.
   */
  export function shelfWheelTarget(e: ShelfWheel, from: number, max: number, pageWidth: number): number | null {
    if (e.ctrlKey || max <= 0 || e.deltaY === 0 || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return null;
    const unit = e.deltaMode === 1 ? LINE_PX : e.deltaMode === 2 ? pageWidth : 1;
    const to = Math.min(max, Math.max(0, from + e.deltaY * unit));
    if (Math.abs(to - from) < 0.5) return e.shiftKey ? from : null;
    return to;
  }

  /**
   * The resting offset reached from `from`: the next card edge in the direction of travel
   * (forward 1, back -1), so settling never pulls against the turn. `offsets` is ascending.
   */
  export function alignedOffset(offsets: readonly number[], from: number, direction: 1 | -1): number {
    if (offsets.length === 0) return from;
    if (direction > 0) return offsets.find((o) => o >= from - 0.5) ?? offsets[offsets.length - 1]!;
    return [...offsets].reverse().find((o) => o <= from + 0.5) ?? offsets[0]!;
  }

  /**
   * Where an arrow takes the shelf: about 80% of the visible width onwards, ending on a card
   * edge and always moving by at least one card while there is room.
   */
  export function arrowTarget(offsets: readonly number[], from: number, direction: 1 | -1, visibleWidth: number): number {
    const goal = from + direction * visibleWidth * 0.8;
    if (direction > 0) {
      const ahead = offsets.filter((o) => o > from + 0.5);
      if (ahead.length === 0) return from;
      return ahead.filter((o) => o <= goal + 0.5).at(-1) ?? ahead[0]!;
    }
    const behind = offsets.filter((o) => o < from - 0.5);
    if (behind.length === 0) return from;
    return behind.find((o) => o >= goal - 0.5) ?? behind.at(-1)!;
  }

  /**
   * Right padding that makes the far end a resting place too: scrolled all the way, a card
   * still sits on the page's left edge, and the last card clears the `minPad` fade. Null
   * when the row fits or no padding can do it.
   */
  export function shelfEndPadding(
    trackWidth: number,
    visibleWidth: number,
    stride: number,
    count: number,
    minPad: number
  ): number | null {
    if (count < 2 || stride <= 0 || trackWidth + minPad <= visibleWidth) return null;
    const steps = Math.min(count - 1, Math.ceil((trackWidth - visibleWidth + minPad) / stride));
    const pad = steps * stride + visibleWidth - trackWidth;
    return pad > 0 ? pad : null;
  }
</script>

<script lang="ts">
  import { t } from "../i18n/index.svelte.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint } from "./format.js";
  import Icon from "./Icon.svelte";
  import type { Tile } from "./TileGrid.svelte";

  interface Props {
    heading: string;
    tiles: readonly Tile[];
    mockMode: boolean;
    smoothScroll?: boolean;
  }

  let { heading, tiles, mockMode, smoothScroll = true }: Props = $props();

  /** The right edge that fades out; the last card clears it when the shelf is at its end. */
  const END_FADE_PX = 48;
  /** A wheel pause this long ends the turn, and the shelf settles on a card edge. */
  const WHEEL_SETTLE_MS = 160;
  /** Snapping comes back after a settle even if the browser never reports arriving. */
  const SNAP_RESUME_MS = 700;
  /** Wheel steps at least this long are animated; shorter ones already arrive as a stream. */
  const SMOOTH_STEP_PX = 48;

  let scroller = $state<HTMLDivElement | null>(null);
  let canPrev = $state(false);
  let canNext = $state(false);
  let endPad = $state<number | null>(null);

  // One wheel turn: where it is heading and which way. Plain variables, handled in events.
  let wheelTarget: number | null = null;
  let wheelDirection: 1 | -1 = 1;
  let settleTimer: ReturnType<typeof setTimeout> | null = null;
  let resumeTimer: ReturnType<typeof setTimeout> | null = null;
  let resumeAt: number | null = null;

  const behavior = (): ScrollBehavior => (smoothScroll ? "smooth" : "auto");

  /** Card edges as scroll offsets (a card on the page's left edge) and the page-side width. */
  function geometry(el: HTMLDivElement) {
    const cards = el.querySelectorAll<HTMLElement>(":scope > .shelf-card");
    const max = Math.max(0, el.scrollWidth - el.clientWidth);
    const first = cards[0];
    if (!first) return { offsets: [0], max, padLeft: 0, visible: el.clientWidth, cards };
    const padLeft = first.offsetLeft;
    const offsets = [...new Set(Array.from(cards, (c) => Math.min(max, Math.max(0, c.offsetLeft - padLeft))))].sort(
      (a, b) => a - b
    );
    return { offsets, max, padLeft, visible: el.clientWidth - padLeft, cards };
  }

  function updateScrollBounds() {
    const el = scroller;
    if (!el) {
      canPrev = false;
      canNext = false;
      return;
    }
    const max = el.scrollWidth - el.clientWidth;
    canPrev = el.scrollLeft > 1;
    canNext = el.scrollLeft < max - 1;
    if (resumeAt !== null && Math.abs(el.scrollLeft - resumeAt) < 1) resumeSnap();
  }

  function measure() {
    const el = scroller;
    if (!el) return;
    const { cards, padLeft, visible } = geometry(el);
    const first = cards[0];
    const last = cards[cards.length - 1];
    const second = cards[1];
    const next =
      first && last && second
        ? shelfEndPadding(last.offsetLeft + last.offsetWidth - padLeft, visible, second.offsetLeft - first.offsetLeft, cards.length, END_FADE_PX)
        : null;
    if (next === null ? endPad !== null : endPad === null || Math.abs(next - endPad) > 0.5) endPad = next;
    updateScrollBounds();
  }

  function resumeSnap() {
    if (resumeTimer) clearTimeout(resumeTimer);
    resumeTimer = null;
    resumeAt = null;
    scroller?.classList.remove("is-wheeling");
  }

  /** Ends a wheel turn early (an arrow was pressed, or the shelf is going away). */
  function cancelWheel() {
    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = null;
    wheelTarget = null;
    resumeSnap();
  }

  function settle() {
    settleTimer = null;
    const el = scroller;
    const from = wheelTarget;
    wheelTarget = null;
    if (!el || from === null) return resumeSnap();
    const target = alignedOffset(geometry(el).offsets, from, wheelDirection);
    if (Math.abs(target - el.scrollLeft) < 1) return resumeSnap();
    // Snapping comes back once the shelf has arrived, so it cannot pull the other way.
    resumeAt = target;
    resumeTimer = setTimeout(resumeSnap, SNAP_RESUME_MS);
    el.scrollTo({ left: target, behavior: behavior() });
  }

  /** Vertical wheel over the shelf turns it sideways while it can still move that way. */
  function onWheel(e: WheelEvent) {
    const el = scroller;
    if (!el) return;
    const from = wheelTarget ?? el.scrollLeft;
    const to = shelfWheelTarget(e, from, el.scrollWidth - el.clientWidth, el.clientWidth);
    if (to === null) return;
    e.preventDefault();
    if (Math.abs(to - from) < 0.5) return;
    wheelDirection = to > from ? 1 : -1;
    wheelTarget = to;
    if (resumeTimer) clearTimeout(resumeTimer);
    resumeTimer = null;
    resumeAt = null;
    // Set on the element right away: the scroll below must not be snapped back.
    el.classList.add("is-wheeling");
    // Wheel notches glide; the fine, frequent steps of a trackpad or smooth wheel follow at once.
    el.scrollTo({ left: to, behavior: Math.abs(to - from) >= SMOOTH_STEP_PX ? behavior() : "auto" });
    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = setTimeout(settle, WHEEL_SETTLE_MS);
  }

  function page(direction: 1 | -1) {
    const el = scroller;
    if (!el) return;
    cancelWheel();
    const { offsets, visible } = geometry(el);
    const target = arrowTarget(offsets, el.scrollLeft, direction, visible);
    if (Math.abs(target - el.scrollLeft) >= 0.5) el.scrollTo({ left: target, behavior: behavior() });
  }

  $effect(() => {
    void tiles.length;
    const el = scroller;
    if (!el) return;
    measure();
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : null;
    ro?.observe(el);
    // Not passive: the page must not also scroll when the shelf takes the turn.
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      ro?.disconnect();
      el.removeEventListener("wheel", onWheel);
      cancelWheel();
    };
  });
</script>

<!-- A horizontal row of covers that bleeds to the window edge. The wheel, the arrows and
     native sideways gestures move it; it rests with a card on the page's left edge. -->
{#if tiles.length > 0}
  <div class="browse-block">
    <div class="browse-block-head">
      <h2 class="type-lg font-semibold">{heading}</h2>
      {#if canPrev || canNext}
        <div class="flex gap-1">
          <button
            type="button"
            class="btn-icon"
            disabled={!canPrev}
            aria-label={t.common.shelfPrevious}
            onclick={() => page(-1)}
            data-testid="cover-shelf-prev"
          >
            <Icon name="chevronLeft" />
          </button>
          <button
            type="button"
            class="btn-icon"
            disabled={!canNext}
            aria-label={t.common.shelfNext}
            onclick={() => page(1)}
            data-testid="cover-shelf-next"
          >
            <Icon name="chevronRight" />
          </button>
        </div>
      {/if}
    </div>
    <div
      bind:this={scroller}
      class="shelf bleed"
      style:padding-right={endPad === null ? undefined : `${endPad}px`}
      onscroll={updateScrollBounds}
      data-testid="cover-shelf"
    >
      {#each tiles as tile (tile.key)}
        <button type="button" class="shelf-card" title={tile.title} onclick={tile.onOpen} data-testid={tile.testId}>
          <Cover cover={tile.cover} seed={tile.key} label={tile.title} {mockMode} class="shelf-cover surface-rounded-lg" />
          <span class="min-w-0">
            <span class="block truncate type-base font-medium" lang={detectCjkLangHint(tile.title)}>{tile.title}</span>
            {#if tile.subtitle}
              <span class="block truncate type-sm" style="color: var(--text-secondary);">{tile.subtitle}</span>
            {/if}
          </span>
        </button>
      {/each}
    </div>
  </div>
{/if}
