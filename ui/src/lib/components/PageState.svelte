<script lang="ts">
  import type { Snippet } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";

  interface Props {
    view: PageView;
    /** Test id prefix, e.g. "library" renders "library-state-empty". */
    name: string;
    emptyText?: string;
    onRetry?: () => void;
    onOpenOfficialPlayer?: () => void;
    children: Snippet;
  }

  let { view, name, emptyText, onRetry, onOpenOfficialPlayer, children }: Props = $props();
</script>

{#if view.cached}
  <div
    role="status"
    class="px-4 py-2 surface-rounded-md border type-xs"
    style="background-color: var(--bg-elevated); border-color: var(--status-info); color: var(--text-secondary);"
    data-testid={`${name}-state-cached`}
  >
    {#if view.error}
      {t.states.refreshFailedNotice.replace("{code}", view.error.code)}
      {#if onRetry}
        <button type="button" class="btn-ghost ml-2" onclick={onRetry}>{t.states.retry}</button>
      {/if}
    {:else}
      {t.states.cachedNotice}
    {/if}
  </div>
{/if}

{#if view.kind === "ready"}
  {@render children()}
{:else if view.kind === "loading"}
  <div class="state-block" role="status" aria-busy="true" data-testid={`${name}-state-loading`}>
    <span class="type-sm" style="color: var(--text-secondary);">{t.states.loading}</span>
  </div>
{:else if view.kind === "empty"}
  <div class="state-block" data-testid={`${name}-state-empty`}>
    <div class="type-base font-semibold">{t.states.emptyTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">{emptyText ?? t.states.emptyDescription}</p>
  </div>
{:else if view.kind === "offline"}
  <div class="state-block" role="status" data-testid={`${name}-state-offline`}>
    <div class="type-base font-semibold" style="color: var(--status-warning);">{t.states.offlineTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">{t.states.offlineDescription}</p>
    {#if onRetry}
      <button type="button" class="btn-secondary" onclick={onRetry}>{t.states.retry}</button>
    {/if}
  </div>
{:else if view.kind === "authExpired"}
  <div class="state-block" role="alert" data-testid={`${name}-state-auth-expired`}>
    <div class="type-base font-semibold" style="color: var(--status-danger);">{t.states.authExpiredTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">{t.states.authExpiredDescription}</p>
    {#if onOpenOfficialPlayer}
      <button type="button" class="btn-primary" onclick={onOpenOfficialPlayer}>{t.states.openOfficialPlayer}</button>
    {/if}
  </div>
{:else if view.kind === "unsupported"}
  <div class="state-block" role="status" data-testid={`${name}-state-unsupported`}>
    <div class="type-base font-semibold">{t.states.unsupportedTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">
      {view.error?.safeMessage ?? t.states.unsupportedDescription}
    </p>
  </div>
{:else}
  <div class="state-block" role="alert" data-testid={`${name}-state-error`}>
    <div class="type-base font-semibold" style="color: var(--status-danger);">
      {t.states.errorTitle}{view.error ? t.common.parenthetical.replace("{text}", view.error.code) : ""}
    </div>
    {#if view.error}
      <p class="type-sm m-0" style="color: var(--text-secondary);">{view.error.safeMessage}</p>
    {/if}
    {#if onRetry}
      <button type="button" class="btn-secondary" onclick={onRetry}>{t.states.retry}</button>
    {/if}
  </div>
{/if}
