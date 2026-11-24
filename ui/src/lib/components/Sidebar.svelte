<script lang="ts">
  import { glass } from "../glass/liquid-glass.js";
  import { t, type NavView } from "../i18n/index.svelte.js";
  import type { PlaybackStatus, PlayerConnectionState } from "@ytmcp/protocol";
  import BrandMark from "../assets/BrandMark.svelte";
  import Icon, { type IconName } from "./Icon.svelte";
  import PlaybackIndicator, { indicatorState } from "./PlaybackIndicator.svelte";

  interface Props {
    activeView: NavView | null;
    collapsed: boolean;
    /** Labels and shortcut hints are hidden or fading out; defaults to `collapsed`. */
    textHidden?: boolean;
    mockMode: boolean;
    playback?: PlaybackStatus | null;
    connection?: PlayerConnectionState | null;
    /** The app's decorative motion policy, for the Now Playing item's playback indicator. */
    motion?: boolean;
    /** Reduced motion: the mark only twinkles its star, the ring stays still. */
    reducedMotion?: boolean;
    onNavigate: (view: NavView) => void;
    onToggleCollapse: () => void;
  }

  let {
    activeView,
    collapsed,
    textHidden = undefined,
    mockMode,
    playback = null,
    connection = null,
    motion = false,
    reducedMotion = false,
    onNavigate,
    onToggleCollapse,
  }: Props = $props();

  const showIndicator = $derived(indicatorState(playback, connection) !== null);
  const hideText = $derived(textHidden ?? collapsed);

  type NavItem = { readonly id: NavView; readonly icon: IconName; readonly shortcut?: string };

  const navGroups: readonly (readonly NavItem[])[] = [
    [
      { id: "home", icon: "home" },
      { id: "search", icon: "search", shortcut: "Ctrl+F" },
    ],
    [
      { id: "library", icon: "library" },
      { id: "liked", icon: "heart" },
      { id: "recent", icon: "clock" },
    ],
    [
      { id: "now-playing", icon: "disc" },
      { id: "queue", icon: "queue" },
      { id: "lyrics", icon: "lyrics", shortcut: "Ctrl+L" },
    ],
    [{ id: "agent-activity", icon: "dj" }],
  ];

  const settingsItem: NavItem = { id: "settings", icon: "gear" };

  // The mark's one-off flourish: the ring turns once (900 ms) and the star twinkles (180 ms out,
  // then a 240 ms spring back). Clicks while it runs are ignored, so turns never stack.
  const RING_MS = 900;
  const TWINKLE_PEAK_MS = 180;
  const REDUCED_TWINKLE_MS = 140;
  let turns = $state(0);
  let twinkle = $state(false);
  let markBusy = false;
  let markTimers: ReturnType<typeof setTimeout>[] = [];

  function playMark() {
    if (markBusy) return;
    markBusy = true;
    if (!reducedMotion) turns += 1;
    twinkle = true;
    const peak = reducedMotion ? REDUCED_TWINKLE_MS : TWINKLE_PEAK_MS;
    markTimers = [
      setTimeout(() => (twinkle = false), peak),
      setTimeout(() => (markBusy = false), reducedMotion ? peak * 2 : RING_MS),
    ];
  }

  $effect(() => () => markTimers.forEach(clearTimeout));
</script>

{#snippet navButton(item: NavItem)}
  {@const isActive = activeView === item.id}
  {@const label = t.nav[item.id]}
  {@const indicator = item.id === "now-playing" && showIndicator}
  <button
    type="button"
    class="nav-item sidebar-item lg-press"
    aria-current={isActive ? "page" : undefined}
    aria-label={collapsed ? label : undefined}
    title={collapsed ? label : undefined}
    onclick={() => onNavigate(item.id)}
    data-testid={`nav-${item.id}`}
  >
    <!-- The current page is marked by its fill and an accent icon (the DJ's own colour on its page); the label stays primary. -->
    {#if indicator}
      <span class="sidebar-glyph"><PlaybackIndicator {playback} {connection} {motion} /></span>
    {:else}
      <Icon
        name={item.icon}
        filled={isActive}
        style={isActive ? `color: var(${item.id === "agent-activity" ? "--agent" : "--accent"});` : undefined}
      />
    {/if}
    <span class="sidebar-text">
      <span class="sidebar-label">{label}</span>
      {#if item.shortcut}
        <span class="sidebar-hint tabular-nums">{item.shortcut}</span>
      {/if}
    </span>
  </button>
{/snippet}

<!-- Floating glass panel inside its frame: while the sidebar collapses or expands, the frame's
     clip slides and the panel stays put (sidebar.css), so the icon column never moves and only
     the labels leave. -->
<div class="sidebar-frame">
  <aside
    class="app-sidebar lg lg--sidebar surface-rounded-xl select-none"
    class:is-text-hidden={hideText}
    aria-label={t.app.sidebarLabel}
    data-testid="app-sidebar"
    {@attach glass}
  >
    <div class="sidebar-head">
      <button
        type="button"
        class="sidebar-mark"
        aria-label={t.app.name}
        onclick={playMark}
        data-testid="brand-mark"
      >
        <BrandMark {turns} {twinkle} />
      </button>
      <span class="sidebar-text sidebar-brand">
        <span class="sidebar-name">{t.app.name}</span>
        {#if mockMode}
          <span class="chip chip-accent">{t.app.mockBadge}</span>
        {/if}
      </span>
    </div>

    <!-- Top padding keeps nav-home's :focus-visible ring inside the scroll clip. -->
    <nav class="sidebar-nav" aria-label={t.app.sidebarLabel}>
      {#each navGroups as group, groupIdx (groupIdx)}
        {#if groupIdx > 0}
          <div class="nav-divider" role="separator" aria-hidden="true"></div>
        {/if}
        {#each group as item (item.id)}
          {@render navButton(item)}
        {/each}
      {/each}
    </nav>

    <!-- The nav column's flex fills the space above, so the foot needs no hairline of its own. -->
    <div class="sidebar-foot">
      <button
        type="button"
        class="nav-item sidebar-item sidebar-collapse lg-press"
        aria-label={collapsed ? t.app.expandSidebar : t.app.collapseSidebar}
        title={collapsed ? t.app.expandSidebar : undefined}
        aria-expanded={!collapsed}
        onclick={onToggleCollapse}
        data-testid="sidebar-collapse-btn"
      >
        <Icon name="chevronLeft" />
        <span class="sidebar-text"><span class="sidebar-label">{t.app.collapseShort}</span></span>
      </button>
      {@render navButton(settingsItem)}
    </div>
  </aside>
</div>
