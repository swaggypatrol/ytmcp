<script lang="ts" module>
  import type { PlaybackStatus, PlayerConnectionState, Track } from "@ytmcp/protocol";

  /**
   * A like toggle for each row; pass it only while likes can be written. `liked` is what was last
   * asked for, `pending` marks it as unconfirmed, and `toggle` stays live while pending: the
   * owner turns a click during a write into the next request.
   */
  export interface TrackRowLike {
    liked: (track: Track) => boolean | null;
    pending: (track: Track) => boolean;
    toggle: (track: Track) => void;
  }

  /** What the current row's playback indicator reflects. */
  export interface TrackRowPlayback {
    playback: PlaybackStatus | null | undefined;
    connection?: PlayerConnectionState | null;
    motion: boolean;
  }

  const PAGE_SCROLLER = 'main[data-testid="main-scroll-region"]';
  /** Height of a collection's condensed header strip; rows revealed by the keyboard stay below it. */
  export const STRIP_PX = 48;
  const DEFAULT_ROW_PX = 56;

  /** The window's one vertical scroll container (the document when rendered outside the app shell). */
  export function pageScroller(el: Element): HTMLElement {
    return el.closest<HTMLElement>(PAGE_SCROLLER) ?? (document.scrollingElement as HTMLElement | null) ?? document.documentElement;
  }

  function isDocumentScroller(root: HTMLElement): boolean {
    return root === document.documentElement || root === document.scrollingElement;
  }

  /** Visible height of the scroller (jsdom and unlaid-out pages report 0: use the window). */
  function viewportHeight(root: HTMLElement): number {
    if (isDocumentScroller(root)) return window.innerHeight;
    return root.clientHeight > 0 ? root.clientHeight : window.innerHeight;
  }

  /** Where `el` starts inside the scroller's content, in px from the content's top. */
  function offsetInScroller(root: HTMLElement, el: HTMLElement): number {
    const rootTop = isDocumentScroller(root) ? 0 : root.getBoundingClientRect().top + root.clientTop;
    return Math.round(el.getBoundingClientRect().top - rootTop + root.scrollTop);
  }

  function px(value: string): number {
    const n = parseFloat(value);
    return Number.isFinite(n) ? n : 0;
  }
</script>

<script lang="ts">
  import { createVirtualizer } from "@tanstack/svelte-virtual";
  import { tick, untrack, type Snippet } from "svelte";
  import { get } from "svelte/store";
  import { t } from "../i18n/index.svelte.js";
  import { getReturnFrames, type ListPosition } from "../return-frame.js";
  import Cover from "./Cover.svelte";
  import { availabilityBadge, detectCjkLangHint, formatArtists, formatDurationMs, isPlayable } from "./format.js";
  import Icon from "./Icon.svelte";
  import PlaybackIndicator, { indicatorState } from "./PlaybackIndicator.svelte";

  interface Props {
    tracks: readonly Track[];
    entryIds?: readonly (string | undefined)[];
    currentVideoId?: string | null;
    currentEntryId?: string | null;
    mockMode: boolean;
    showHeader?: boolean;
    ariaLabel?: string;
    onPlayTrack: (track: Track, index: number, entryId?: string) => void;
    /** Opens the track actions menu (also on right click / context-menu key). */
    onTrackActions?: (track: Track, index: number, entryId?: string) => void;
    /** Extra per-row controls before the like and duration cells; gets the row index and roving tabindex. */
    rowActions?: Snippet<[index: number, tabindex: number]>;
    /** Optional content in the shared action column's header. */
    headerActions?: Snippet;
    /** Number of 32 px controls reserved in the action column, including on rows without controls. */
    rowActionCount?: number;
    /** Number shown on the first row (later rows count up from it). */
    firstNumber?: number;
    /** Replaces the playing marker on the current row. */
    currentMarker?: Snippet<[number: number]>;
    /** Shows the playback indicator on the current row (a static marker when absent). */
    nowPlaying?: TrackRowPlayback;
    /** Adds a like toggle column (Regular and wider); pass it only while likes can be written. */
    like?: TrackRowLike;
    /** Whether this list takes part in the lyrics return hand-off (one list per page should). */
    returnFrame?: boolean;
    /** More rows exist; `onLoadMore` is called when the end comes within 1.5 screens. */
    hasMore?: boolean;
    loadingMore?: boolean;
    /** The last page request failed: no automatic retry, an inline Retry row instead. */
    moreFailed?: boolean;
    onLoadMore?: () => void;
    /** Set while rows cannot be started: they stay focusable for the actions menu, but are aria-disabled with this reason as their tooltip. */
    playDisabled?: string | null;
  }

  let {
    tracks,
    entryIds,
    currentVideoId = null,
    currentEntryId = null,
    mockMode,
    showHeader = false,
    ariaLabel = t.track.listLabel,
    onPlayTrack,
    onTrackActions,
    rowActions,
    headerActions,
    rowActionCount = 0,
    firstNumber = 1,
    currentMarker,
    nowPlaying,
    like,
    returnFrame = true,
    hasMore = false,
    loadingMore = false,
    moreFailed = false,
    onLoadMore,
    playDisabled = null,
  }: Props = $props();

  let listEl = $state<HTMLDivElement | null>(null);
  /** The rows' box (below the header row): its start is the virtualizer's scroll margin. */
  let bodyEl = $state<HTMLDivElement | null>(null);
  let scroller = $state<HTMLElement | null>(null);
  /**
   * The roving tab stop: the focused row's key, and its index when it was set (a hint for
   * finding it again, and the place to fall back to when the row is gone).
   */
  let focus = $state.raw<{ key: string | null; index: number }>({ key: null, index: 0 });
  /** Row height for the current layout tier, read from `--track-row-h` (52 compact, 56 otherwise). */
  let rowPx = $state(DEFAULT_ROW_PX);

  /** Stable row key: queue occurrence id when present, never videoId alone. */
  function rowKey(index: number): string {
    const eid = entryIds?.[index];
    if (eid) return eid;
    const track = tracks[index];
    return track ? `${track.videoId}::${index}` : `row-${index}`;
  }

  const virtualizer = createVirtualizer<HTMLElement, HTMLDivElement>({
    count: 0,
    getScrollElement: () => scroller,
    estimateSize: () => rowPx,
    overscan: 8,
    initialRect: { width: 800, height: 720 },
    observeElementRect: (instance, cb) => {
      const el = instance.scrollElement;
      if (!el) return;
      const measure = () => cb({ width: el.clientWidth || 800, height: viewportHeight(el) });
      measure();
      if (typeof ResizeObserver === "undefined") return undefined;
      const ro = new ResizeObserver(measure);
      ro.observe(el);
      return () => ro.disconnect();
    },
    getItemKey: (index) => rowKey(index),
  });

  /**
   * The tab stop's row in the current rows, found again by key when rows move (the current queue
   * entry advances, a row above goes), so focus stays on the same track. When that row is gone
   * the row now in its place takes the tab stop; DOM focus is never moved from here.
   */
  const focusedIndex = $derived.by(() => {
    const count = tracks.length;
    if (count === 0) return 0;
    const { key, index } = focus;
    if (key !== null) {
      for (const near of [index, index - 1, index + 1]) {
        if (near >= 0 && near < count && rowKey(near) === key) return near;
      }
      const found = indexOfKey(key);
      if (found >= 0) return found;
    }
    return Math.min(index, count - 1);
  });

  function setFocus(index: number) {
    focus = { key: rowKey(index), index };
  }

  // Keep the hint where the row is now, and hand the tab stop to the row that took its place
  // when it is gone, so later changes start from there.
  $effect.pre(() => {
    const index = focusedIndex;
    untrack(() => {
      if (focus.key === null || tracks.length === 0) return;
      if (focus.index !== index || focus.key !== rowKey(index)) setFocus(index);
    });
  });

  // Row count and keys. The key function is new only when the rows change, never on a focus
  // move: the virtualizer rebuilds every row's measurement whenever it gets a new one.
  $effect(() => {
    const count = tracks.length;
    void entryIds;
    const el = scroller;
    get(virtualizer).setOptions({
      count,
      getScrollElement: () => el,
      getItemKey: (index) => rowKey(index),
    });
  });

  /** The list's start in the scroller, measured now (content above it may have changed). */
  function listOffset(): number {
    const root = scroller;
    const el = bodyEl;
    return root && el ? offsetInScroller(root, el) : get(virtualizer).options.scrollMargin;
  }

  /**
   * Reads the tier's row height, the list's offset in the page and the scroller's insets. A row
   * height change keeps the row at the top of the window in place instead of jumping.
   */
  function syncLayout() {
    const root = scroller;
    const el = listEl;
    const body = bodyEl;
    if (!root || !el || !body) return;
    const v = get(virtualizer);
    const nextRow = Math.round(px(getComputedStyle(el).getPropertyValue("--track-row-h"))) || DEFAULT_ROW_PX;
    const margin = offsetInScroller(root, body);
    const cs = getComputedStyle(root);
    const padStart = px(cs.scrollPaddingTop) || STRIP_PX;
    const padEnd = px(cs.scrollPaddingBottom) || px(cs.paddingBottom);
    const options = v.options;
    if (
      nextRow === rowPx &&
      margin === options.scrollMargin &&
      padStart === options.scrollPaddingStart &&
      padEnd === options.scrollPaddingEnd
    ) {
      return;
    }
    const keep = nextRow !== rowPx ? capturePosition() : null;
    rowPx = nextRow;
    v.setOptions({ scrollMargin: margin, scrollPaddingStart: padStart, scrollPaddingEnd: padEnd, estimateSize: () => nextRow });
    if (!keep) return;
    v.measure();
    const top = root.scrollTop - margin;
    if (top >= 0 && top < tracks.length * rowPx) root.scrollTop = anchoredTop(keep);
  }

  $effect(() => {
    const el = listEl;
    if (!el) return;
    const root = pageScroller(el);
    scroller = root;
    untrack(syncLayout);
    if (typeof ResizeObserver === "undefined") return;
    // Anything above the list that changes height also changes the page content's height.
    const ro = new ResizeObserver(() => syncLayout());
    ro.observe(el);
    ro.observe(root);
    const content = root.firstElementChild;
    if (content && !isDocumentScroller(root)) ro.observe(content);
    return () => ro.disconnect();
  });

  // Position hand-off for a page left for lyrics and shown again (see return-frame.ts). Only a
  // list taking part on mount claims a pending position; registration follows `returnFrame`.
  const frames = getReturnFrames();
  let restore = untrack(() => returnFrame) ? (frames?.claim() ?? null) : null;

  /** The page position relative to this list's rows; the offset is negative above its first row. */
  function capturePosition(): ListPosition | null {
    const root = scroller;
    const el = listEl;
    if (!root || !el || tracks.length === 0) return null;
    const top = root.scrollTop - listOffset();
    const anchorIndex = Math.min(tracks.length - 1, Math.max(0, Math.floor(top / rowPx)));
    const focused = el.contains(document.activeElement);
    return {
      anchorKey: rowKey(anchorIndex),
      offsetPx: top - anchorIndex * rowPx,
      scrollTop: root.scrollTop,
      focusKey: focused ? rowKey(focusedIndex) : null,
      focusIndex: focused ? focusedIndex : null,
    };
  }

  function indexOfKey(key: string): number {
    for (let i = 0; i < tracks.length; i++) if (rowKey(i) === key) return i;
    return -1;
  }

  /** Scroller offset that puts the anchored row back where it was, or the old offset when it is gone. */
  function anchoredTop(position: ListPosition): number {
    const anchor = indexOfKey(position.anchorKey);
    const top = anchor >= 0 ? listOffset() + anchor * rowPx + position.offsetPx : position.scrollTop;
    return Math.max(0, top);
  }

  function nextFrame(cb: () => void) {
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(cb);
    else setTimeout(cb, 16);
  }

  /**
   * Scrolls the page back to the anchored row and refocuses the row. `restored` (the app's own
   * raw restore) runs first so the anchored offset wins; one frame later the offset is put back
   * if a late raw write replaced it.
   */
  async function applyPosition(position: ListPosition, restored: () => void) {
    // The rows' spacer gets its height in the next render; before that the browser would clamp.
    await tick();
    const root = scroller;
    if (!root) return restored();
    restored();
    const target = anchoredTop(position);
    root.scrollTop = target;
    const applied = root.scrollTop;
    // Let the virtualizer read the new offset now, so the rows to focus are rendered.
    root.dispatchEvent(new Event("scroll"));
    nextFrame(() => {
      if (listEl?.isConnected && root.scrollTop !== applied && Math.abs(root.scrollTop - target) > 1) {
        root.scrollTop = target;
        root.dispatchEvent(new Event("scroll"));
      }
    });
    if (position.focusIndex === null) return;
    const keyed = position.focusKey === null ? -1 : indexOfKey(position.focusKey);
    setFocus(keyed >= 0 ? keyed : Math.min(tracks.length - 1, position.focusIndex));
    await tick();
    listEl?.querySelector<HTMLElement>(`[data-row-index="${focusedIndex}"]`)?.focus({ preventScroll: true });
  }

  $effect(() => (returnFrame ? frames?.register(capturePosition) : undefined));

  // Applied once the rows it refers to have loaded.
  $effect(() => {
    const root = scroller;
    if (!restore || !root || tracks.length === 0) return;
    const { position, restored } = restore;
    restore = null;
    untrack(syncLayout);
    void applyPosition(position, restored);
  });

  /** Focuses a row once it is rendered (a far jump renders it after the scroll event). */
  function focusRow(idx: number, retries = 2) {
    const row = listEl?.querySelector<HTMLElement>(`[data-row-index="${idx}"]`);
    if (row) row.focus({ preventScroll: true });
    else if (retries > 0) nextFrame(() => focusRow(idx, retries - 1));
  }

  function moveFocus(idx: number, align: "auto" | "start" | "end" = "auto") {
    setFocus(idx);
    get(virtualizer).scrollToIndex(idx, { align });
    void tick().then(() => focusRow(idx));
  }

  function handleKeydown(e: KeyboardEvent) {
    if (tracks.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveFocus(Math.min(tracks.length - 1, focusedIndex + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveFocus(Math.max(0, focusedIndex - 1));
    } else if (e.key === "Home") {
      e.preventDefault();
      moveFocus(0, "start");
    } else if (e.key === "End") {
      e.preventDefault();
      moveFocus(tracks.length - 1, "end");
    } else if ((e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) && onTrackActions) {
      const track = tracks[focusedIndex];
      if (track) {
        e.preventDefault();
        onTrackActions(track, focusedIndex, entryIds?.[focusedIndex]);
      }
    }
  }

  const virtualItems = $derived($virtualizer.getVirtualItems());
  const totalSize = $derived($virtualizer.getTotalSize());
  const margin = $derived($virtualizer.options.scrollMargin);
  const activeTabIndex = $derived.by(() => {
    if (tracks.length === 0) return -1;
    if (virtualItems.some((item) => item.index === focusedIndex)) return focusedIndex;
    const top = (scroller?.scrollTop ?? 0) + ($virtualizer.options.scrollPaddingStart ?? 0);
    return virtualItems.find((item) => item.end > top)?.index ?? virtualItems[0]?.index ?? 0;
  });
  const hasRowActions = $derived(rowActionCount > 0 || !!(rowActions || headerActions));
  const indicator = $derived(nowPlaying ? indicatorState(nowPlaying.playback, nowPlaying.connection) : null);

  const tailNeeded = $derived(!!onLoadMore && (hasMore || moreFailed));
  /**
   * The tail keeps its space after the last page lands (an empty last page would otherwise
   * shorten the page under a listener at its end) until it is below the window.
   */
  let tailHeld = $state(false);
  $effect(() => {
    if (tailNeeded) {
      tailHeld = true;
      return;
    }
    const root = scroller;
    if (!tailHeld || !root) return;
    const tailTop = margin + totalSize;
    // Released once it is below the window, or once the page is long enough that removing it
    // cannot pull the page up (a list that ends inside the window).
    const release = () => {
      const below = tailTop >= root.scrollTop + viewportHeight(root);
      const noClamp = root.scrollTop <= root.scrollHeight - viewportHeight(root) - rowPx;
      if (below || noClamp) tailHeld = false;
    };
    release();
    root.addEventListener("scroll", release, { passive: true });
    return () => root.removeEventListener("scroll", release);
  });

  /** Row count a page was last asked for at; cleared once the request is under way. */
  let askedAt = -1;

  // Fetch the next page while its end is still 1.5 screens away. Re-checked whenever the
  // visible range, the rows or the loading state change (a short first page never scrolls).
  $effect(() => {
    void virtualItems;
    const count = tracks.length;
    if (loadingMore) askedAt = -1;
    if (!onLoadMore || !hasMore || loadingMore || moreFailed || count === 0 || askedAt === count) return;
    const root = untrack(() => scroller);
    if (!root) return;
    const viewport = viewportHeight(root);
    const end = margin + totalSize;
    if (end - (root.scrollTop + viewport) < viewport * 1.5) {
      askedAt = count;
      untrack(onLoadMore);
    }
  });
</script>

<div
  class="track-list"
  bind:this={listEl}
  class:has-like={!!like}
  class:has-row-actions={hasRowActions}
  class:has-more-actions={!!onTrackActions}
  style:--tl-action-count={Math.max(1, rowActionCount)}
>
  {#if showHeader && tracks.length > 0}
    <div class="track-list-header" aria-hidden="true">
      <span class="tl-main">
        <span class="tl-num">{t.track.columnIndex}</span>
        <!-- The cell is box-identical to a row's title cell; only its label is indented past the cover. -->
        <span class="tl-title"><span class="tl-title-head">{t.track.columnTitle}</span></span>
        <span class="tl-album">{t.track.columnAlbum}</span>
      </span>
      {#if hasRowActions}<span class="tl-actions">{@render headerActions?.()}</span>{/if}
      {#if like}<span class="tl-like"></span>{/if}
      <span class="tl-dur">{t.track.columnDuration}</span>
      {#if onTrackActions}<span class="tl-more"></span>{/if}
    </div>
  {/if}

  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    bind:this={bodyEl}
    class="track-list-body"
    style="height: {totalSize}px;"
    role="list"
    aria-label={ariaLabel}
    onkeydown={handleKeydown}
    data-testid="virtual-track-list"
  >
    {#each virtualItems as row (row.key)}
      {@const track = tracks[row.index]}
      {#if track}
        {@const entryId = entryIds?.[row.index]}
        {@const isCurrent = entryId ? entryId === currentEntryId : track.videoId === currentVideoId}
        {@const badge = availabilityBadge(track)}
        {@const playable = isPlayable(track)}
        {@const tabindex = activeTabIndex === row.index ? 0 : -1}
        <div
          role="listitem"
          class="tl-row"
          class:is-current={isCurrent}
          class:is-unavailable={!playable}
          style="height: {row.size}px; transform: translateY({row.start - margin}px);"
          data-row-key={String(row.key)}
          data-testid="virtual-track-row"
          oncontextmenu={(e) => {
            if (!onTrackActions) return;
            e.preventDefault();
            onTrackActions(track, row.index, entryId);
          }}
        >
          <button
            type="button"
            class="tl-main"
            {tabindex}
            aria-current={isCurrent ? "true" : undefined}
            aria-disabled={playable && !playDisabled ? undefined : "true"}
            title={playDisabled ?? undefined}
            data-row-index={row.index}
            onclick={() => {
              setFocus(row.index);
              if (playable && !playDisabled) onPlayTrack(track, row.index, entryId);
            }}
            onfocus={() => setFocus(row.index)}
          >
            <span class="tl-num">
              {#if isCurrent && currentMarker}
                {@render currentMarker(firstNumber + row.index)}
              {:else if isCurrent && nowPlaying}
                {#if indicator}
                  <PlaybackIndicator playback={nowPlaying.playback} connection={nowPlaying.connection} motion={nowPlaying.motion} />
                {:else}
                  {firstNumber + row.index}
                {/if}
              {:else if isCurrent}
                <Icon name="playing" />
              {:else}
                {firstNumber + row.index}
              {/if}
            </span>
            <span class="tl-title">
              <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="tl-cover" />
              <span class="tl-text">
                <span class="tl-title-text" lang={detectCjkLangHint(track.title)} title={track.title}>{track.title}</span>
                <span class="tl-sub" class:is-warning={!!badge}>{badge ?? formatArtists(track)}</span>
              </span>
            </span>
            <span class="tl-album" lang={detectCjkLangHint(track.album?.title ?? "")}>{track.album?.title ?? ""}</span>
          </button>
          {#if hasRowActions}
            <div class="tl-actions">{@render rowActions?.(row.index, tabindex)}</div>
          {/if}
          {#if like}
            {@const status = like.liked(track)}
            {@const liked = status === true}
            {@const busy = like.pending(track)}
            <!-- Unknown like status (e.g. queue tracks): an empty cell keeps the columns aligned. -->
            <span class="tl-like">
              {#if status !== null}
              <button
                type="button"
                class="btn-icon btn-icon-sm"
                class:btn-icon-active={liked}
                {tabindex}
                aria-pressed={liked}
                aria-busy={busy}
                data-pending={busy || undefined}
                aria-label={(liked ? t.track.unlikeFor : t.track.likeFor).replace("{title}", track.title)}
                title={liked ? t.track.unlike : t.track.like}
                onclick={() => like.toggle(track)}
                data-testid="track-like-btn"
              >
                <Icon name="heart" filled={liked} />
              </button>
              {/if}
            </span>
          {/if}
          <span class="tl-dur">{formatDurationMs(track.durationMs)}</span>
          {#if onTrackActions}
            <span class="tl-more">
              <button
                type="button"
                class="btn-icon btn-icon-sm"
                {tabindex}
                aria-label={t.track.actionsFor.replace("{title}", track.title)}
                title={t.track.actions}
                onclick={() => onTrackActions(track, row.index, entryId)}
                data-testid="track-actions-btn"
              >
                <Icon name="more" />
              </button>
            </span>
          {/if}
        </div>
      {/if}
    {/each}
  </div>

  {#if tailNeeded || tailHeld}
    <!-- Always as tall as one row, so its states never move the page; blank once held after the last page. -->
    <div class="tl-tail" style="height: {rowPx}px;" data-testid="track-list-tail" aria-hidden={tailNeeded ? undefined : "true"}>
      {#if tailNeeded}
        <!-- Stays mounted while more can load, so screen readers announce the loading note and the failure. -->
        <span class="tl-tail-note" class:tl-tail-failed={moreFailed} role="status" aria-live="polite">
          {moreFailed ? t.track.loadMoreFailed : loadingMore ? t.track.loadingMore : ""}
        </span>
        {#if moreFailed}
          <button type="button" class="btn-ghost" onclick={onLoadMore} data-testid="track-list-retry">{t.states.retry}</button>
        {/if}
      {/if}
    </div>
  {/if}
</div>
