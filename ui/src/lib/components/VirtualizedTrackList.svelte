<script lang="ts">
  import { createVirtualizer } from "@tanstack/svelte-virtual";
  import type { Track } from "@ytm-glass/protocol";
  import type { Snippet } from "svelte";
  import { get } from "svelte/store";
  import { t } from "../i18n/index.svelte.js";
  import Cover from "./Cover.svelte";
  import { availabilityBadge, detectCjkLangHint, formatArtists, formatDurationMs, isPlayable } from "./format.js";
  import Icon from "./Icon.svelte";

  interface Props {
    tracks: readonly Track[];
    entryIds?: readonly (string | undefined)[];
    currentVideoId?: string | null;
    currentEntryId?: string | null;
    mockMode: boolean;
    heightPx?: number;
    showHeader?: boolean;
    ariaLabel?: string;
    onPlayTrack: (track: Track, index: number, entryId?: string) => void;
    /** Opens the track actions menu (also on right click / context-menu key). */
    onTrackActions?: (track: Track, index: number, entryId?: string) => void;
    /** Extra per-row controls before the actions button; gets the row index and roving tabindex. */
    rowActions?: Snippet<[index: number, tabindex: number]>;
  }

  let {
    tracks,
    entryIds,
    currentVideoId = null,
    currentEntryId = null,
    mockMode,
    heightPx = 440,
    showHeader = false,
    ariaLabel = t.track.listLabel,
    onPlayTrack,
    onTrackActions,
    rowActions,
  }: Props = $props();

  let scrollEl = $state<HTMLDivElement | null>(null);
  let focusedIndex = $state(0);

  /** Stable row key: queue occurrence id when present, never videoId alone. */
  function rowKey(index: number): string {
    const eid = entryIds?.[index];
    if (eid) return eid;
    const t = tracks[index];
    return t ? `${t.videoId}::${index}` : `row-${index}`;
  }

  const virtualizer = createVirtualizer<HTMLDivElement, HTMLDivElement>({
    count: 0,
    getScrollElement: () => scrollEl,
    estimateSize: () => 58,
    overscan: 6,
    initialRect: { width: 800, height: 480 },
    observeElementRect: (instance, cb) => {
      const el = instance.scrollElement;
      if (!el) return;
      const measure = () => {
        const rect = el.getBoundingClientRect();
        cb({
          width: rect.width > 0 ? rect.width : 800,
          height: rect.height > 0 ? rect.height : heightPx,
        });
      };
      measure();
      if (typeof ResizeObserver !== "undefined") {
        const ro = new ResizeObserver(measure);
        ro.observe(el);
        return () => ro.disconnect();
      }
      return undefined;
    },
    getItemKey: (index) => rowKey(index),
  });

  $effect(() => {
    const count = tracks.length;
    const el = scrollEl;
    if (focusedIndex >= count) focusedIndex = Math.max(0, count - 1);
    get(virtualizer).setOptions({
      count,
      getScrollElement: () => el,
      getItemKey: (index) => rowKey(index),
    });
  });

  function moveFocus(idx: number, align: "auto" | "start" | "end" = "auto") {
    focusedIndex = idx;
    get(virtualizer).scrollToIndex(idx, { align });
    queueMicrotask(() => {
      scrollEl?.querySelector<HTMLElement>(`[data-row-index="${idx}"]`)?.focus();
    });
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
  const activeTabIndex = $derived.by(() => {
    if (tracks.length === 0) return -1;
    if (virtualItems.some((item) => item.index === focusedIndex)) return focusedIndex;
    const top = scrollEl?.scrollTop ?? 0;
    return (
      virtualItems.find((item) => item.start + item.size > top)?.index ??
      virtualItems[0]?.index ??
      0
    );
  });
</script>

<div
  class="border surface-rounded-lg overflow-hidden"
  style="border-color: var(--border-subtle); background-color: var(--bg-elevated);"
>
  {#if showHeader && tracks.length > 0}
    <div class="track-list-header type-xs" aria-hidden="true">
      <span class="w-10 text-right shrink-0">{t.track.columnIndex}</span>
      <span class="flex-1 min-w-0 pl-12">{t.track.columnTitle}</span>
      <span class="w-12 text-right shrink-0">{t.track.columnDuration}</span>
      {#if onTrackActions}<span class="w-8 shrink-0"></span>{/if}
    </div>
  {/if}

  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    bind:this={scrollEl}
    class="overflow-y-auto relative"
    style="height: {heightPx}px;"
    role="list"
    aria-label={ariaLabel}
    onkeydown={handleKeydown}
    data-testid="virtual-track-list"
  >
    <div class="relative w-full" style="height: {totalSize}px;">
      {#each virtualItems as row (row.key)}
        {@const track = tracks[row.index]}
        {#if track}
          {@const entryId = entryIds?.[row.index]}
          {@const isCurrent = entryId ? entryId === currentEntryId : track.videoId === currentVideoId}
          {@const badge = availabilityBadge(track)}
          {@const playable = isPlayable(track)}
          <div
            role="listitem"
            class="track-row absolute left-0 top-0 w-full flex items-center gap-2 pr-2 border-b"
            class:track-row-current={isCurrent}
            style="height: {row.size}px; transform: translateY({row.start}px); border-color: var(--border-subtle); opacity: {playable ? 1 : 0.6};"
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
              class="flex-1 min-w-0 h-full flex items-center gap-3 pl-3.5 text-left cursor-pointer"
              tabindex={activeTabIndex === row.index ? 0 : -1}
              aria-current={isCurrent ? "true" : undefined}
              aria-disabled={playable ? undefined : "true"}
              data-row-index={row.index}
              onclick={() => {
                focusedIndex = row.index;
                if (playable) onPlayTrack(track, row.index, entryId);
              }}
              onfocus={() => {
                focusedIndex = row.index;
              }}
            >
              <span
                class="w-10 flex items-center justify-end type-xs tabular-nums shrink-0"
                style="color: {isCurrent ? 'var(--accent)' : 'var(--text-muted)'};"
              >
                {#if isCurrent}
                  <Icon name="playing" />
                {:else}
                  {row.index + 1}
                {/if}
              </span>
              <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="h-9 w-9 rounded" />
              <span class="min-w-0 flex-1">
                <span
                  class="block truncate type-sm font-medium"
                  style="color: {isCurrent ? 'var(--accent)' : 'var(--text-primary)'};"
                  lang={detectCjkLangHint(track.title)}
                  title={track.title}
                >
                  {track.title}
                </span>
                <span class="block truncate type-xs" style="color: var(--text-secondary);">
                  {formatArtists(track)}{track.album?.title ? ` · ${track.album.title}` : ""}
                </span>
              </span>
              {#if badge}
                <span
                  class="px-2 py-0.5 rounded type-xs shrink-0"
                  style="background-color: var(--bg-subtle); color: var(--status-warning);"
                >
                  {badge}
                </span>
              {/if}
              <span class="w-12 text-right type-xs tabular-nums shrink-0" style="color: var(--text-secondary);">
                {formatDurationMs(track.durationMs)}
              </span>
            </button>
            {@render rowActions?.(row.index, activeTabIndex === row.index ? 0 : -1)}
            {#if onTrackActions}
              <button
                type="button"
                class="btn-icon shrink-0"
                tabindex={activeTabIndex === row.index ? 0 : -1}
                aria-label={t.track.actionsFor.replace("{title}", track.title)}
                title={t.track.actions}
                onclick={() => onTrackActions(track, row.index, entryId)}
                data-testid="track-actions-btn"
              >
                <Icon name="more" />
              </button>
            {/if}
          </div>
        {/if}
      {/each}
    </div>
  </div>
</div>
