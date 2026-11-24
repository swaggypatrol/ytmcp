<script lang="ts">
  import type { Snippet } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import { queryErrorDetail, queryErrorMessage } from "../i18n/query-errors.js";

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

  const errorDetail = $derived(queryErrorDetail(view.error));
</script>

{#if view.kind === "ready"}
  {@render children()}
{:else if view.kind === "loading"}
  <div class="state-block" role="status" aria-busy="true" data-testid={`${name}-state-loading`}>
    <span class="type-sm" style="color: var(--text-secondary);">{t.states.loading}</span>
  </div>
{:else if view.kind === "empty"}
  <div class="state-block" title={errorDetail} data-testid={`${name}-state-empty`}>
    <div class="type-base font-semibold">{t.states.emptyTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">{emptyText ?? t.states.emptyDescription}</p>
  </div>
{:else if view.kind === "offline"}
  <div class="state-block" role="status" title={errorDetail} data-testid={`${name}-state-offline`}>
    <div class="type-base font-semibold" style="color: var(--status-warning);">{t.states.offlineTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">{t.states.offlineDescription}</p>
    {#if onRetry}
      <button type="button" class="btn-secondary" onclick={onRetry}>{t.states.retry}</button>
    {/if}
  </div>
{:else if view.kind === "authExpired"}
  <div class="state-block" role="alert" title={errorDetail} data-testid={`${name}-state-auth-expired`}>
    <div class="type-base font-semibold" style="color: var(--status-danger);">{t.states.authExpiredTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">
      {queryErrorMessage(view.error?.code, t.states.authExpiredDescription)}
    </p>
    {#if onOpenOfficialPlayer}
      <button type="button" class="btn-primary" onclick={onOpenOfficialPlayer}>{t.states.openOfficialPlayer}</button>
    {/if}
  </div>
{:else if view.kind === "unsupported"}
  <div class="state-block" role="status" title={errorDetail} data-testid={`${name}-state-unsupported`}>
    <div class="type-base font-semibold">{t.states.unsupportedTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">
      {t.states.unsupportedDescription}
    </p>
  </div>
{:else}
  <div class="state-block" role="alert" title={errorDetail} data-testid={`${name}-state-error`}>
    <div class="type-base font-semibold" style="color: var(--status-danger);">{t.states.errorTitle}</div>
    <p class="type-sm m-0" style="color: var(--text-secondary);">
      {queryErrorMessage(view.error?.code, t.states.errorDescription)}
    </p>
    {#if onRetry}
      <button type="button" class="btn-secondary" onclick={onRetry}>{t.states.retry}</button>
    {/if}
  </div>
{/if}
