<script lang="ts" module>
  /** A box in viewport pixels; a point is a box with no size. */
  export interface Box {
    readonly left: number;
    readonly top: number;
    readonly right: number;
    readonly bottom: number;
  }

  /** Where the menu sits: `top` when it opens downward, `bottom` (from the viewport's foot) upward. */
  export interface MenuPlacement {
    readonly left: number;
    readonly width: number;
    readonly top: number | null;
    readonly bottom: number | null;
    readonly maxHeight: number;
    /** The anchor's point in the menu's own box, so the menu grows out of its trigger. */
    readonly origin: string;
  }

  /** Space between the trigger and the menu, and between the menu and the content area's edge. */
  const MENU_GAP = 6;
  const MENU_EDGE = 8;
  const MENU_WIDTH = 300;
  /** The side is chosen for at least the chooser's height (header, filter, six playlists), so a
   *  short first step never leaves the chooser squeezed into a little room. */
  const MENU_PLAN_HEIGHT = 360;
  /** A trigger counts only when the menu opens this soon after it. */
  const TRIGGER_WINDOW_MS = 1000;
  /** A scroll this soon after the listener's wheel, touch or scroll key is one they made. */
  const USER_SCROLL_WINDOW_MS = 300;
  /** Keys that scroll the page when focus is outside the menu. */
  const SCROLL_KEYS = new Set(["PageUp", "PageDown", "Home", "End", "ArrowUp", "ArrowDown", " "]);

  const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), high);

  /**
   * Places the menu by its anchor inside `bounds`: below the anchor when it fits, else above, else
   * on the side with more room (it then scrolls inside). It lines up with the anchor's nearer side
   * and never leaves `bounds`. Pure, so it can be checked with numbers:
   * `placeMenu(anchorRect, { width: 300, height: el.offsetHeight }, contentBounds, innerHeight)`.
   */
  export function placeMenu(
    anchor: Box,
    size: { readonly width: number; readonly height: number },
    bounds: Box,
    viewportHeight: number
  ): MenuPlacement {
    const width = Math.min(size.width, bounds.right - bounds.left);
    const anchorX = (anchor.left + anchor.right) / 2;
    const alignRight = anchorX >= (bounds.left + bounds.right) / 2;
    const left = clamp(alignRight ? anchor.right - width : anchor.left, bounds.left, bounds.right - width);
    const roomBelow = bounds.bottom - (anchor.bottom + MENU_GAP);
    const roomAbove = anchor.top - MENU_GAP - bounds.top;
    const below = size.height <= roomBelow || (size.height > roomAbove && roomBelow >= roomAbove);
    const originX = `${Math.round(clamp(anchorX - left, 0, width))}px`;
    if (below) {
      const top = clamp(anchor.bottom + MENU_GAP, bounds.top, bounds.bottom);
      return { left, width, top, bottom: null, maxHeight: bounds.bottom - top, origin: `${originX} 0` };
    }
    const edge = clamp(anchor.top - MENU_GAP, bounds.top, bounds.bottom);
    return { left, width, top: null, bottom: viewportHeight - edge, maxHeight: edge - bounds.top, origin: `${originX} 100%` };
  }

  // The last click, right-click or key press, so the menu can find the control (or the point) it
  // was opened from without every list passing it along.
  let lastTrigger: { type: string; target: Element; point: { x: number; y: number } | null; at: number } | null = null;
  // A press on the open menu's own trigger closes it; the click that follows must not reopen it.
  let swallowClickOn: Element | null = null;

  function rememberTrigger(event: Event) {
    // No click follows a right-click or a key press, so a pending swallow would catch a later one.
    if (event.type !== "click") swallowClickOn = null;
    if (!(event.target instanceof Element)) return;
    const pointer = event.type === "contextmenu" && event instanceof MouseEvent && (event.clientX !== 0 || event.clientY !== 0);
    lastTrigger = {
      type: event.type,
      target: event.target,
      point: pointer ? { x: (event as MouseEvent).clientX, y: (event as MouseEvent).clientY } : null,
      at: performance.now(),
    };
  }

  function swallowClick(event: Event) {
    const target = swallowClickOn;
    swallowClickOn = null;
    if (target && event.target instanceof Node && target.contains(event.target)) {
      event.stopImmediatePropagation();
      event.preventDefault();
    }
  }

  // Scrollers still moving (a wheel notch's glide, a smooth scroll) until their scroll ends.
  const gliding = new WeakSet<EventTarget>();

  if (typeof document !== "undefined") {
    for (const type of ["click", "contextmenu", "keydown"]) document.addEventListener(type, rememberTrigger, true);
    window.addEventListener("click", swallowClick, true);
    window.addEventListener("pointerdown", () => (swallowClickOn = null), true);
    document.addEventListener("scroll", (event) => event.target && gliding.add(event.target), { capture: true, passive: true });
    document.addEventListener("scrollend", (event) => event.target && gliding.delete(event.target), { capture: true, passive: true });
  }

  /** The trigger's scroller that is still gliding, if any: a press made mid-glide rides on it. */
  function glidingScroller(source: Element): Element | null {
    for (let el = source.parentElement; el; el = el.parentElement) if (gliding.has(el)) return el;
    return null;
  }

  /**
   * What just opened the menu: `opener` gets focus back on close (and anchors the menu when there
   * is no point), `point` is a right-click's position, and `toggle` is true only when a click on
   * the opener opened it, so only then does a press on the opener just close the menu. A
   * right-click opens on whatever sits under the pointer (a row's play button, the page), so its
   * opener is the control focused at the time and pressing it afterwards works as usual.
   */
  function currentTrigger(): {
    opener: HTMLElement | null;
    point: { x: number; y: number } | null;
    toggle: boolean;
    /** What the menu stands by on the page: the opener, or what a right-click was on. */
    source: Element | null;
  } {
    const recent = lastTrigger && performance.now() - lastTrigger.at < TRIGGER_WINDOW_MS ? lastTrigger : null;
    const focused = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null;
    if (recent?.type === "click") {
      const clicked = recent.target.closest<HTMLElement>('button, a[href], [tabindex]:not([tabindex="-1"])');
      if (clicked?.isConnected) return { opener: clicked, point: null, toggle: true, source: clicked };
    }
    const point = recent?.point ?? null;
    return { opener: focused, point, toggle: false, source: point ? recent!.target : focused };
  }

  /** The content area: right of the sidebar, above the player bar, inset from both. */
  function contentBounds(): Box {
    const content = document.querySelector(".app-content")?.getBoundingClientRect();
    const dock = document.querySelector(".app-player-dock")?.getBoundingClientRect();
    const left = (content?.left ?? 0) + MENU_EDGE;
    const bottom = (dock && dock.height > 0 ? dock.top : window.innerHeight) - MENU_EDGE;
    return { left, top: MENU_EDGE, right: Math.max(left, window.innerWidth - MENU_EDGE), bottom: Math.max(MENU_EDGE, bottom) };
  }
</script>

<script lang="ts">
  import type { EnqueuePosition, PlaylistSummary, QueryError, Track } from "@ytmcp/protocol";
  import { tick, untrack } from "svelte";
  import type { Gate } from "../capabilities.js";
  import type { PageView } from "../catalog/page-state.js";
  import { glass } from "../glass/liquid-glass.js";
  import { OverlayPresence } from "../glass/overlay-motion.svelte.js";
  import { t } from "../i18n/index.svelte.js";
  import { queryErrorDetail, queryErrorMessage } from "../i18n/query-errors.js";
  import Cover from "./Cover.svelte";
  import { formatArtists, isPlayable } from "./format.js";
  import Icon, { type IconName } from "./Icon.svelte";
  import PageState from "./PageState.svelte";

  interface Props {
    track: Track;
    active?: boolean;
    openKey?: number;
    liked: boolean | null;
    mockMode?: boolean;
    enqueueGate: Gate;
    likeGate: Gate;
    playlistGate: Gate;
    canOpenAlbum: boolean;
    canOpenArtist: boolean;
    /** True while a write for this track is in flight. */
    pending: boolean;
    playlistsView: PageView;
    playlists: readonly PlaylistSummary[];
    playlistsHasMore?: boolean;
    playlistsLoadingMore?: boolean;
    playlistsMoreError?: QueryError | null;
    /** "pick" opens straight on the playlist chooser (Now playing's Add to playlist). */
    startAt?: "menu" | "pick";
    /** Escape, a press outside, a scroll the listener made, a scroll or list update that moved the trigger or took it away, or a resize asks to close. */
    onClose: () => void;
    onPlay: () => void;
    onEnqueue: (position: EnqueuePosition) => void;
    onSetLike: (liked: boolean) => void;
    onRequestPlaylists: () => void;
    onLoadMorePlaylists?: () => void;
    onRetryPlaylists?: () => void;
    onAddToPlaylist: (playlist: PlaylistSummary) => void;
    onOpenAlbum: (albumId: string) => void;
    onOpenArtist: (artistId: string) => void;
  }

  let {
    track,
    active = true,
    openKey = 0,
    liked,
    mockMode = false,
    enqueueGate,
    likeGate,
    playlistGate,
    canOpenAlbum,
    canOpenArtist,
    pending,
    playlistsView,
    playlists,
    playlistsHasMore = false,
    playlistsLoadingMore = false,
    playlistsMoreError = null,
    startAt = "menu",
    onClose,
    onPlay,
    onEnqueue,
    onSetLike,
    onRequestPlaylists,
    onLoadMorePlaylists,
    onRetryPlaylists,
    onAddToPlaylist,
    onOpenAlbum,
    onOpenArtist,
  }: Props = $props();

  let step = $state<"menu" | "pick">("menu");
  let playlistFilter = $state("");

  const albumId = $derived(track.album?.id ?? track.album?.browseId ?? null);
  const artistLinks = $derived((track.artists ?? []).filter((a) => a.id));
  const hasNavLinks = $derived((albumId && track.album && canOpenAlbum) || (canOpenArtist && artistLinks.length > 0));
  const filteredPlaylists = $derived(
    playlistFilter.trim()
      ? playlists.filter((pl) => pl.title.toLowerCase().includes(playlistFilter.trim().toLowerCase()))
      : playlists
  );

  const presence = new OverlayPresence();
  $effect(() => () => {
    if (active) restoreFocus(true);
    presence.dispose();
  });
  let trigger: ReturnType<typeof currentTrigger>;
  let bounds: Box;
  let anchor: Box;
  /** Where the trigger's element was when the menu opened, to tell whether a scroll or an update moved it. */
  let sourceAt: Box | null = null;
  let width = $state(MENU_WIDTH);
  let menuEl = $state<HTMLDivElement | null>(null);
  let placement = $state<MenuPlacement | null>(null);

  /** Read the opener and content geometry when the menu opens, including during an outro. */
  function prepareMenu() {
    trigger = currentTrigger();
    bounds = contentBounds();
    anchor = trigger.point
      ? { left: trigger.point.x, right: trigger.point.x, top: trigger.point.y, bottom: trigger.point.y }
      : (trigger.opener?.getBoundingClientRect() ?? {
          left: (bounds.left + bounds.right) / 2,
          right: (bounds.left + bounds.right) / 2,
          top: bounds.top + 56,
          bottom: bounds.top + 56,
        });
    width = Math.min(MENU_WIDTH, bounds.right - bounds.left);
    sourceAt = trigger.source?.isConnected ? trigger.source.getBoundingClientRect() : null;
    riding = trigger.source && sourceAt ? glidingScroller(trigger.source) : null;
  }

  /**
   * The scroller still gliding from the listener's last wheel notch when the menu opened. The
   * press that opened the menu is their newest intent, so until that glide ends the menu moves
   * with its trigger instead of closing; a glide cannot be stopped from script in Chromium (a
   * programmatic scroll only nudges the running animation). Input after the open still closes.
   */
  let riding: Element | null = null;

  /** Moves the menu (and its anchor) by as much as its trigger moved, on the side it opened on, and takes the new spot as home. */
  function followSource() {
    const source = trigger?.source;
    if (!source?.isConnected || !sourceAt || !placement) return;
    const now = source.getBoundingClientRect();
    const dx = now.left - sourceAt.left;
    const dy = now.top - sourceAt.top;
    sourceAt = now;
    if (dx === 0 && dy === 0) return;
    anchor = { left: anchor.left + dx, right: anchor.right + dx, top: anchor.top + dy, bottom: anchor.bottom + dy };
    placement = {
      ...placement,
      left: placement.left + dx,
      top: placement.top != null ? placement.top + dy : null,
      bottom: placement.bottom != null ? placement.bottom - dy : null,
    };
  }

  /** After a scroll or a list update: follow a glide the menu opened on, else close if the trigger moved or left. */
  function checkSource() {
    if (riding && trigger.source?.isConnected) followSource();
    else if (sourceMoved()) onClose();
  }

  /** The menu's trigger moved or left the page since it opened, so the menu no longer stands by it. */
  function sourceMoved(): boolean {
    const source = trigger?.source;
    if (!source || !sourceAt) return false;
    if (!source.isConnected) return true;
    const now = source.getBoundingClientRect();
    return Math.abs(now.top - sourceAt.top) > 1 || Math.abs(now.left - sourceAt.left) > 1;
  }

  const FOCUS_FIRST = 'input[type="search"], .menu-item:not(:disabled), .btn-primary:not(:disabled)';
  const FOCUSABLE =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  /** Puts the menu by its trigger once, before it is first painted. */
  function place(node: HTMLDivElement) {
    placement = placeMenu(anchor, { width, height: Math.max(node.offsetHeight, MENU_PLAN_HEIGHT) }, bounds, window.innerHeight);
  }

  /** Moves focus to the step's first useful control (the filter or the first item). */
  async function focusStep(reposition = false) {
    await tick();
    if (!active || !menuEl) return;
    if (reposition) place(menuEl);
    const target = menuEl.querySelector<HTMLElement>(FOCUS_FIRST) ?? menuEl.querySelector<HTMLElement>(FOCUSABLE) ?? menuEl;
    target.focus({ preventScroll: true });
  }

  /** True when the element or an ancestor is inert, where focus() silently does nothing. */
  function isInert(el: HTMLElement): boolean {
    for (let node: HTMLElement | null = el; node; node = node.parentElement) if (node.inert) return true;
    return false;
  }

  /** On close focus goes back to the opener, or to the page title when the opener has gone. */
  function restoreFocus(unmounting = false) {
    const opener = trigger?.opener;
    if (menuEl && document.activeElement && !menuEl.contains(document.activeElement) && document.activeElement !== document.body) return;
    queueMicrotask(() => {
      if (active && !unmounting) return;
      if (opener?.isConnected && !isInert(opener)) opener.focus({ preventScroll: true });
      if (opener && document.activeElement === opener) return;
      const fallback = document.querySelector<HTMLElement>(".app-page h1") ?? document.querySelector<HTMLElement>(".app-scroll");
      if (!fallback) return;
      if (!fallback.hasAttribute("tabindex")) fallback.setAttribute("tabindex", "-1");
      fallback.focus({ preventScroll: true });
    });
  }

  $effect(() => {
    const isOpen = active;
    // A new invocation can select a different row or open straight to the picker.
    void openKey;
    if (!isOpen) {
      untrack(() => {
        restoreFocus();
        presence.hide(menuEl);
      });
      return;
    }
    untrack(() => {
      prepareMenu();
      step = startAt;
      playlistFilter = "";
      presence.show();
      if (step === "pick") onRequestPlaylists();
      void focusStep(true);
    });
    const controller = new AbortController();
    const { signal } = controller;
    document.addEventListener(
      "pointerdown",
      (event) => {
        const target = event.target instanceof Node ? event.target : null;
        if (!target || menuEl?.contains(target)) return;
        // A press on the trigger closes the menu; the click it ends in must not open it again.
        if (trigger.toggle && trigger.opener?.contains(target)) swallowClickOn = trigger.opener;
        onClose();
      },
      { capture: true, signal }
    );
    // A scroll outside the menu (the playlist list inside it scrolls) closes it when the
    // listener made it (wheel, touch, scroll keys; a press on a scrollbar is a press outside) or
    // when it moved the trigger. Scrolls nobody asked for (a smooth-scroll tail, a shelf
    // snapping, a list clamping after an update) leave a menu that still stands by its trigger.
    let userScrollAt = Number.NEGATIVE_INFINITY;
    // The glide the menu opened on ends at its scrollend (or, should none come, within a second).
    const ride = untrack(() => riding);
    if (ride) {
      const settle = () => (riding = null);
      ride.addEventListener("scrollend", settle, { once: true, signal });
      const timer = setTimeout(settle, 1000);
      signal.addEventListener("abort", () => clearTimeout(timer));
    }
    const outside = (event: Event) => !(event.target instanceof Node && menuEl?.contains(event.target));
    const noteUserScroll = (event: Event) => {
      if (outside(event)) userScrollAt = performance.now();
    };
    document.addEventListener("wheel", noteUserScroll, { capture: true, passive: true, signal });
    document.addEventListener("touchmove", noteUserScroll, { capture: true, passive: true, signal });
    document.addEventListener(
      "keydown",
      (event) => {
        if (SCROLL_KEYS.has(event.key)) noteUserScroll(event);
      },
      { capture: true, signal }
    );
    document.addEventListener(
      "scroll",
      (event) => {
        if (!outside(event)) return;
        if (performance.now() - userScrollAt < USER_SCROLL_WINDOW_MS) {
          userScrollAt = Number.NEGATIVE_INFINITY;
          onClose();
        } else checkSource();
      },
      { capture: true, signal }
    );
    window.addEventListener("resize", () => onClose(), { signal });
    // A list update can move the trigger's row or take it away with no scroll at all (an entry
    // before it leaves, the row itself leaves). Rows come and go and are placed by their inline
    // style, so the trigger's scroll region is watched for those changes; each batch checks the
    // trigger once, in the next frame before it is painted. Nothing runs while the page is still.
    const source = untrack(() => trigger.source);
    if (source && sourceAt) {
      let frame = 0;
      const watch = new MutationObserver((records) => {
        if (frame || records.every((record) => menuEl?.contains(record.target))) return;
        frame = requestAnimationFrame(() => {
          frame = 0;
          checkSource();
        });
      });
      watch.observe(source.closest(".app-scroll") ?? document.body, {
        subtree: true,
        childList: true,
        attributes: true,
        attributeFilter: ["style", "class"],
      });
      signal.addEventListener("abort", () => {
        watch.disconnect();
        cancelAnimationFrame(frame);
      });
    }
    return () => controller.abort();
  });

  function selectStep(next: "menu" | "pick") {
    step = next;
    void focusStep();
  }

  /** Keys stay in the menu: Escape closes it, arrows walk its items, Tab cycles inside it. */
  function handleKeydown(event: KeyboardEvent) {
    if (!active) return;
    // Global shortcuts must not fire while the menu has focus.
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (!menuEl) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Home" || event.key === "End") {
      const inField = event.target instanceof HTMLInputElement;
      if (inField && (event.key === "Home" || event.key === "End")) return;
      const items = Array.from(menuEl.querySelectorAll<HTMLElement>(".menu-item:not(:disabled)"));
      if (items.length === 0) return;
      event.preventDefault();
      const at = items.indexOf(document.activeElement as HTMLElement);
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? items.length - 1
            : event.key === "ArrowDown"
              ? (at + 1) % items.length
              : at <= 0
                ? items.length - 1
                : at - 1;
      items[next]!.focus();
      return;
    }
    if (event.key === "Tab") {
      const focusables = Array.from(menuEl.querySelectorAll<HTMLElement>(FOCUSABLE));
      if (focusables.length === 0) {
        event.preventDefault();
        return;
      }
      const first = focusables[0]!;
      const last = focusables[focusables.length - 1]!;
      const active = document.activeElement;
      if (event.shiftKey && (active === first || active === menuEl)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    }
  }
</script>

{#snippet action(
  icon: IconName,
  label: string,
  gate: { enabled: boolean; reason: string | null },
  onclick: () => void,
  testId: string,
  filled = false
)}
  <li>
    <!-- A pending like or add waits only on the two write items; playing, queueing and navigating stay open. -->
    <button
      type="button"
      class="menu-item lg-press"
      disabled={!gate.enabled || (pending && (gate === likeGate || gate === playlistGate))}
      {onclick}
      data-testid={testId}
    >
      <Icon name={icon} {filled} />
      <span class="min-w-0 flex-1 flex flex-col gap-0.5">
        <span class="truncate">{label}</span>
        {#if gate.reason}
          <span class="type-xs" style="color: var(--text-muted);">{gate.reason}</span>
        {/if}
      </span>
    </button>
  </li>
{/snippet}

{#if presence.rendered}
<!-- An L4 menu: above the content area only, grown from its trigger, no scrim. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  bind:this={menuEl}
  class="track-menu lg lg--popover lg-overlay"
  role="dialog"
  aria-label={t.track.actionsFor.replace("{title}", track.title)}
  tabindex="-1"
  style:left={placement ? `${placement.left}px` : undefined}
  style:top={placement?.top != null ? `${placement.top}px` : undefined}
  style:bottom={placement?.bottom != null ? `${placement.bottom}px` : undefined}
  style:width={`${width}px`}
  style:max-height={placement ? `${placement.maxHeight}px` : undefined}
  style:--lg-morph-origin={placement?.origin}
  onkeydown={handleKeydown}
  data-testid="track-actions-menu"
  {@attach place}
  {@attach glass}
>
<div class="track-menu-body lg-overlay-content flex flex-col gap-3" data-testid="track-actions">
  <div class="flex items-center gap-3 pb-2 border-b" style="border-color: var(--border-subtle);">
    <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="h-11 w-11 rounded-lg" />
    <div class="min-w-0 flex-1">
      <div class="type-base font-semibold truncate">{track.title}</div>
      <div class="type-xs truncate" style="color: var(--text-secondary);">{formatArtists(track)}</div>
    </div>
  </div>

  {#if step === "menu"}
    <ul class="flex flex-col gap-1 m-0 p-0 list-none" aria-label={t.track.actions}>
      {@render action("play", t.track.play, { enabled: isPlayable(track), reason: null }, onPlay, "action-play")}
      {@render action("playNext", t.track.playNext, enqueueGate, () => onEnqueue("next"), "action-play-next")}
      {@render action("addToQueue", t.track.addToQueue, enqueueGate, () => onEnqueue("last"), "action-add-to-queue")}
      <li class="menu-divider" role="separator" aria-hidden="true"></li>
      {@render action(
        "heart",
        liked === true ? t.track.unlike : t.track.like,
        likeGate,
        () => onSetLike(liked !== true),
        "action-like",
        liked === true
      )}
      {@render action(
        "library",
        t.track.addToPlaylist,
        playlistGate,
        () => {
          playlistFilter = "";
          selectStep("pick");
          onRequestPlaylists();
        },
        "action-add-to-playlist"
      )}
      {#if hasNavLinks}
        <li class="menu-divider" role="separator" aria-hidden="true"></li>
      {/if}
      {#if albumId && track.album && canOpenAlbum}
        {@render action(
          "disc",
          t.track.goToAlbum.replace("{title}", track.album.title),
          { enabled: true, reason: null },
          () => onOpenAlbum(albumId),
          "action-go-album"
        )}
      {/if}
      {#if canOpenArtist}
        {#each artistLinks as artist (artist.id)}
          {@render action(
            "user",
            t.track.goToArtist.replace("{name}", artist.name),
            { enabled: true, reason: null },
            () => onOpenArtist(artist.id!),
            `action-go-artist-${artist.id}`
          )}
        {/each}
      {/if}
    </ul>
  {:else if step === "pick"}
    <div class="flex items-center justify-between gap-2">
      <h3 class="type-sm font-semibold m-0">{t.track.choosePlaylist}</h3>
      <button type="button" class="btn-ghost" onclick={() => selectStep("menu")}>
        <Icon name="arrowLeft" />
        <span>{t.track.back}</span>
      </button>
    </div>
    {#if playlists.length > 6 || playlistsHasMore}
      <input
        type="search"
        value={playlistFilter}
        placeholder={t.track.filterPlaylists}
        aria-label={t.track.filterPlaylists}
        class="px-3 py-1.5 rounded-lg border type-sm"
        style="background-color: var(--bg-subtle); border-color: var(--border-strong); color: var(--text-primary);"
        oninput={(e) => (playlistFilter = e.currentTarget.value)}
      />
      {#if playlistsHasMore}
        <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="playlist-picker-loaded-hint">
          {t.track.filterLoadedOnly}
        </p>
      {/if}
    {/if}
    <PageState view={playlistsView} name="playlist-picker" emptyText={t.track.noPlaylists} onRetry={onRetryPlaylists}>
      {#if filteredPlaylists.length === 0}
        <p class="type-sm m-0 py-3 text-center" style="color: var(--text-secondary);">{t.track.noMatchingPlaylists}</p>
      {:else}
        <ul class="flex flex-col gap-1 m-0 p-0 list-none max-h-72 overflow-y-auto">
          {#each filteredPlaylists as pl (pl.playlistId)}
            <li>
              <!-- The pick adds at once (the toast offers Undo when it can); no confirmation step. -->
              <button
                type="button"
                class="menu-item lg-press justify-between"
                disabled={pending || !playlistGate.enabled}
                onclick={() => onAddToPlaylist(pl)}
                data-testid={`pick-playlist-${pl.playlistId}`}
              >
                <span class="truncate">{pl.title}</span>
                {#if pl.trackCount != null}
                  <span class="type-xs tabular-nums shrink-0" style="color: var(--text-muted);">
                    {t.common.trackCount.replace("{count}", String(pl.trackCount))}
                  </span>
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      {#if playlistsMoreError}
        <div
          class="flex items-center justify-between gap-2 px-3 py-2 rounded-lg type-xs"
          style="background-color: var(--bg-subtle); border: 1px solid var(--border-subtle); color: var(--status-danger);"
          title={queryErrorDetail(playlistsMoreError)}
          data-testid="playlist-picker-more-error"
        >
          <span>{queryErrorMessage(playlistsMoreError.code, t.library.loadMoreFailed)}</span>
          {#if playlistsHasMore && onLoadMorePlaylists}
            <button
              type="button"
              class="btn-secondary shrink-0"
              data-testid="playlist-picker-load-more"
              disabled={playlistsLoadingMore}
              onclick={onLoadMorePlaylists}
            >
              {playlistsLoadingMore ? t.common.loadingMore : t.states.retry}
            </button>
          {/if}
        </div>
      {:else if playlistsHasMore && onLoadMorePlaylists}
        <div class="pt-1">
          <button
            type="button"
            class="btn-secondary w-full"
            data-testid="playlist-picker-load-more"
            disabled={playlistsLoadingMore}
            onclick={onLoadMorePlaylists}
          >
            {playlistsLoadingMore ? t.common.loadingMore : t.common.loadMore}
          </button>
        </div>
      {/if}
    </PageState>
  {/if}
</div>
</div>

{/if}

<style>
  /* Fixed to the window, so it never enters the page's flow; placed by placeMenu. */
  .track-menu {
    position: fixed;
    z-index: var(--z-overlay);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .track-menu-body {
    min-height: 0;
    overflow-y: auto;
    padding: var(--space-12);
  }
</style>
