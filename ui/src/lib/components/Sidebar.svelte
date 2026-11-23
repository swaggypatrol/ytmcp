<script lang="ts">
  import { glass } from "../glass/liquid-glass.js";
  import { t, type NavView } from "../i18n/index.svelte.js";
  import Icon, { type IconName } from "./Icon.svelte";

  interface Props {
    activeView: NavView | null;
    collapsed: boolean;
    mockMode: boolean;
    isPlaying?: boolean;
    onNavigate: (view: NavView) => void;
    onToggleCollapse: () => void;
  }

  let { activeView, collapsed, mockMode, isPlaying = false, onNavigate, onToggleCollapse }: Props = $props();

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
</script>

{#snippet navButton(item: NavItem)}
  {@const isActive = activeView === item.id}
  {@const label = t.nav[item.id]}
  {@const showPlayingIcon = item.id === "now-playing" && isPlaying}
  <button
    type="button"
    class="nav-item lg-press"
    class:justify-center={collapsed}
    aria-current={isActive ? "page" : undefined}
    aria-label={collapsed ? label : undefined}
    title={collapsed ? label : undefined}
    onclick={() => onNavigate(item.id)}
    data-testid={`nav-${item.id}`}
  >
    <!-- The current page is marked by its fill and an accent icon; the label stays primary. -->
    <Icon
      name={showPlayingIcon ? "playing" : item.icon}
      filled={isActive}
      style={isActive || showPlayingIcon ? "color: var(--accent);" : undefined}
    />
    {#if !collapsed}
      <span class="flex-1 truncate type-sm font-medium">{label}</span>
      {#if item.shortcut}
        <span class="type-xs tabular-nums" style="color: var(--text-muted);">{item.shortcut}</span>
      {/if}
    {/if}
  </button>
{/snippet}

<!-- Floating glass panel; its width follows --content-inset (see .app-shell). -->
<aside
  class="app-sidebar lg lg--sidebar surface-rounded-xl select-none"
  aria-label={t.app.sidebarLabel}
  data-testid="app-sidebar"
  {@attach glass}
>
  <div class="flex items-center gap-2 px-3 pt-3.5 pb-2.5" class:justify-between={!collapsed} class:justify-center={collapsed}>
    {#if !collapsed}
      <div class="min-w-0 pl-1.5">
        <div class="flex items-center gap-2">
          <!-- Original neutral mark: two overlapping glass panes drawn with theme tokens. -->
          <svg class="h-7 w-7 shrink-0" viewBox="0 0 28 28" aria-hidden="true" data-testid="brand-mark">
            <rect x="0.75" y="0.75" width="26.5" height="26.5" rx="8" fill="var(--bg-subtle)" stroke="var(--border-strong)" />
            <rect x="6" y="10" width="11" height="11" rx="3" fill="var(--text-primary)" fill-opacity="0.22" />
            <rect x="11" y="6.5" width="11" height="11" rx="3" fill="none" stroke="var(--text-primary)" stroke-width="1.5" />
          </svg>
          <span class="truncate font-semibold type-base">{t.app.name}</span>
        </div>
        {#if mockMode}
          <span class="chip chip-accent mt-1.5 inline-block">{t.app.mockBadge}</span>
        {/if}
      </div>
    {/if}

    <button
      type="button"
      class="btn-icon lg-press"
      aria-label={collapsed ? t.app.expandSidebar : t.app.collapseSidebar}
      aria-expanded={!collapsed}
      onclick={onToggleCollapse}
      data-testid="sidebar-collapse-btn"
    >
      <Icon name={collapsed ? "chevronRight" : "chevronLeft"} />
    </button>
  </div>

  <!-- Top padding keeps nav-home's :focus-visible ring inside the scroll clip. -->
  <nav class="flex-1 overflow-y-auto px-2 py-1.5 flex flex-col gap-0.5" aria-label={t.app.sidebarLabel}>
    {#each navGroups as group, groupIdx (groupIdx)}
      {#if groupIdx > 0}
        <div class="nav-divider" role="separator" aria-hidden="true"></div>
      {/if}
      {#each group as item (item.id)}
        {@render navButton(item)}
      {/each}
    {/each}
  </nav>

  <div class="px-2 pt-1 pb-2.5">
    <div class="nav-divider mt-0 mb-1.5" role="separator" aria-hidden="true"></div>
    {@render navButton(settingsItem)}
  </div>
</aside>
