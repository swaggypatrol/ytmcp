<script lang="ts">
  import type { AppSettings, AppSnapshot, ThemeMode } from "@ytm-glass/protocol";
  import { LOCALE_NAMES, LOCALES, t, type Locale } from "../i18n/index.svelte.js";
  import type { CacheClearOutcome } from "../ipc/client.js";
  import AccessibleDialog from "./AccessibleDialog.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    resnapshotCount: number;
    onUpdateSettings: (next: AppSettings) => void;
    onOpenDiagnostics: () => void;
    onOpenOfficialPlayer?: () => void;
    /** Resolves once core confirms, rejects or fails to confirm the clear. */
    onClearCache: () => Promise<CacheClearOutcome>;
    onLogout: () => void;
  }

  let {
    snapshot,
    resnapshotCount,
    onUpdateSettings,
    onOpenDiagnostics,
    onOpenOfficialPlayer,
    onClearCache,
    onLogout,
  }: Props = $props();

  let logoutConfirmOpen = $state(false);
  type CacheClearState = { readonly status: "idle" | "pending" } | CacheClearOutcome;
  let cacheClear = $state<CacheClearState>({ status: "idle" });

  const trayAvailable = $derived(snapshot?.hostHealth.trayAvailable === true);
  const auth = $derived(snapshot?.authState ?? null);

  /** UI scales the host applies; a larger stored value is shown as the largest. */
  const uiScales = [100, 125, 150] as const;
  const shownUiScale = $derived(Math.min(150, snapshot?.settings.uiScalePercent ?? 100));

  /**
   * Changes sent but not yet seen in the snapshot. Core replaces the whole settings object, so
   * each submission carries them until the snapshot shows each value; a later snapshot then
   * wins for that field again.
   */
  let unconfirmed: Partial<AppSettings> = {};
  $effect(() => {
    const current = snapshot?.settings;
    if (!current) return;
    unconfirmed = Object.fromEntries(
      Object.entries(unconfirmed).filter(([key, value]) => current[key as keyof AppSettings] !== value)
    );
  });

  function patchSettings(partial: Partial<AppSettings>) {
    if (!snapshot) return;
    unconfirmed = { ...unconfirmed, ...partial };
    onUpdateSettings({ ...snapshot.settings, ...unconfirmed });
  }

  async function handleClearCache() {
    if ("status" in cacheClear && cacheClear.status === "pending") return;
    cacheClear = { status: "pending" };
    cacheClear = await onClearCache();
  }
</script>

<section class="flex flex-col gap-6" aria-labelledby="settings-heading" data-testid="view-settings">
  <h1 id="settings-heading" class="type-xl font-bold tracking-tight">{t.settings.title}</h1>

  {#if !snapshot}
    <div class="state-block" role="status">{t.states.loading}</div>
  {:else}
    <div class="grid grid-cols-1 lg:grid-cols-2 gap-5">
      <div class="card p-5 flex flex-col gap-4">
        <h2 class="type-lg font-semibold m-0">{t.settings.appearanceSection}</h2>
        <label class="setting-row">
          <span>{t.settings.languageLabel}</span>
          <select
            class="select"
            value={snapshot.settings.uiLanguage ?? "en"}
            onchange={(e) => patchSettings({ uiLanguage: e.currentTarget.value as Locale })}
            data-testid="setting-language-select"
          >
            {#each LOCALES as locale (locale)}
              <option value={locale} lang={locale}>{LOCALE_NAMES[locale]}</option>
            {/each}
          </select>
        </label>
        <label class="setting-row">
          <span>{t.settings.themeLabel}</span>
          <select
            class="select"
            value={snapshot.settings.theme}
            onchange={(e) => patchSettings({ theme: e.currentTarget.value as ThemeMode })}
            data-testid="setting-theme-select"
          >
            <option value="dark">{t.settings.themes.dark}</option>
            <option value="light">{t.settings.themes.light}</option>
            <option value="system">{t.settings.themes.system}</option>
          </select>
        </label>
        <label class="setting-row">
          <span>{t.settings.reduceMotion}</span>
          <input
            type="checkbox"
            role="switch"
            class="toggle"
            checked={snapshot.settings.reduceMotion}
            onchange={(e) => patchSettings({ reduceMotion: e.currentTarget.checked })}
            data-testid="setting-reduce-motion"
          />
        </label>
        <label class="setting-row">
          <span>{t.settings.reduceTransparency}</span>
          <input
            type="checkbox"
            role="switch"
            class="toggle"
            checked={snapshot.settings.reduceTransparency}
            onchange={(e) => patchSettings({ reduceTransparency: e.currentTarget.checked })}
            data-testid="setting-reduce-transparency"
          />
        </label>
        <label class="setting-row">
          <span>{t.settings.uiScale}</span>
          <select
            class="select tabular-nums"
            value={String(shownUiScale)}
            onchange={(e) => patchSettings({ uiScalePercent: Number(e.currentTarget.value) })}
            data-testid="setting-ui-scale"
          >
            {#each uiScales as scale (scale)}
              <option value={String(scale)}>{scale}%</option>
            {/each}
          </select>
        </label>
      </div>

      <div class="card p-5 flex flex-col gap-4">
        <h2 class="type-lg font-semibold m-0">{t.settings.behaviorSection}</h2>
        <label class="setting-row">
          <span>{t.settings.lrclibEnabled}</span>
          <input
            type="checkbox"
            role="switch"
            class="toggle"
            checked={snapshot.settings.lrclibEnabled}
            onchange={(e) => patchSettings({ lrclibEnabled: e.currentTarget.checked })}
            data-testid="setting-lrclib"
          />
        </label>
        <label class="setting-row">
          <span>{t.settings.autoplayOnLaunch}</span>
          <input
            type="checkbox"
            role="switch"
            class="toggle"
            checked={snapshot.settings.autoplayOnLaunch}
            onchange={(e) => patchSettings({ autoplayOnLaunch: e.currentTarget.checked })}
            data-testid="setting-autoplay"
          />
        </label>
        <label class="setting-row">
          <span>{t.settings.trackNotifications}</span>
          <input
            type="checkbox"
            role="switch"
            class="toggle"
            checked={snapshot.settings.trackNotifications}
            onchange={(e) => patchSettings({ trackNotifications: e.currentTarget.checked })}
            data-testid="setting-track-notifications"
          />
        </label>
        <div class="flex flex-col gap-1.5">
          <label class="setting-row" class:opacity-60={!trayAvailable}>
            <span>{t.settings.closeToTray}</span>
            <input
              type="checkbox"
              role="switch"
              class="toggle"
              checked={trayAvailable && snapshot.settings.closeToTray}
              disabled={!trayAvailable}
              aria-describedby={trayAvailable ? undefined : "tray-unavailable-note"}
              onchange={(e) => patchSettings({ closeToTray: e.currentTarget.checked })}
              data-testid="setting-close-to-tray"
            />
          </label>
          {#if !trayAvailable}
            <p id="tray-unavailable-note" class="type-xs m-0" style="color: var(--text-muted);" data-testid="tray-unavailable-note">
              {t.settings.trayUnavailable}
            </p>
          {/if}
        </div>
      </div>
    </div>

    <div class="card p-5 flex flex-wrap items-center justify-between gap-4">
      <div class="flex flex-col gap-1.5">
        <h2 class="type-lg font-semibold m-0">{t.settings.accountSection}</h2>
        <div class="flex flex-wrap items-center gap-2 type-sm" style="color: var(--text-secondary);" data-testid="settings-account-state">
          <span>
            {t.settings.accountState}{t.common.colon}{t.settings.authStates[auth?.state ?? "unknown"]}
            {#if auth?.channelLabel} · {auth.channelLabel}{/if}
          </span>
          {#if auth?.state === "authenticated"}
            <span
              class="chip"
              style="color: {auth.playerCatalogMatched ? 'var(--status-success)' : 'var(--status-warning)'};"
              data-testid="settings-account-match"
            >
              {auth.playerCatalogMatched ? t.settings.accountMatched : t.settings.accountUnmatched}
            </span>
          {/if}
        </div>
      </div>
      <div class="flex flex-wrap gap-2">
        {#if onOpenOfficialPlayer}
          <button
            type="button"
            class="btn-secondary"
            onclick={onOpenOfficialPlayer}
            data-testid="settings-open-official-player"
          >
            <Icon name="externalLink" />
            <span>{t.settings.openOfficialPlayer}</span>
          </button>
        {/if}
        <button
          type="button"
          class="btn-secondary"
          style="color: var(--status-danger);"
          disabled={auth?.state !== "authenticated" && auth?.state !== "expired"}
          onclick={() => (logoutConfirmOpen = true)}
          data-testid="settings-logout"
        >
          {t.settings.logout}
        </button>
      </div>
    </div>

    <div class="card p-5 flex flex-wrap items-center justify-between gap-4">
      <div class="flex flex-col gap-1">
        <h2 class="type-lg font-semibold m-0">{t.settings.diagnosticsSection}</h2>
        <div class="type-xs tabular-nums" style="color: var(--text-secondary);">
          #{snapshot.revision} · {snapshot.coreEpoch} · {t.dialog.resnapshotCount} {resnapshotCount}
        </div>
        <div role="status" class="type-xs mt-1" data-testid="settings-cache-status">
          {#if "status" in cacheClear}
            {#if cacheClear.status === "pending"}
              <span style="color: var(--text-secondary);" data-testid="settings-cache-pending">{t.settings.cacheClearing}</span>
            {/if}
          {:else if cacheClear.outcome === "confirmed"}
            <span class="flex items-center gap-1.5" style="color: var(--status-success);" data-testid="settings-cache-cleared">
              <Icon name="check" />
              <span>{t.settings.cacheCleared}</span>
            </span>
          {:else if cacheClear.outcome === "failed"}
            <span style="color: var(--status-danger);" data-testid="settings-cache-failed">
              {t.settings.cacheClearFailed.replace("{message}", cacheClear.error.safeMessage)}
            </span>
          {:else}
            <span style="color: var(--status-warning);" data-testid="settings-cache-unconfirmed">{t.settings.cacheClearUnconfirmed}</span>
          {/if}
        </div>
      </div>
      <div class="flex flex-wrap gap-2">
        <button type="button" class="btn-primary" onclick={onOpenDiagnostics} data-testid="settings-open-diagnostics">
          {t.settings.openDiagnosticsDialog}
        </button>
        <button
          type="button"
          class="btn-secondary"
          disabled={"status" in cacheClear && cacheClear.status === "pending"}
          aria-busy={"status" in cacheClear && cacheClear.status === "pending"}
          onclick={() => void handleClearCache()}
          data-testid="settings-clear-cache"
        >
          {t.settings.clearCache}
        </button>
      </div>
    </div>
  {/if}
</section>

<AccessibleDialog open={logoutConfirmOpen} title={t.settings.logoutConfirmTitle} onClose={() => (logoutConfirmOpen = false)}>
  <div class="flex flex-col gap-4">
    <p class="type-sm m-0">{t.settings.logoutConfirmBody}</p>
    <div class="flex justify-end gap-2">
      <button type="button" class="btn-secondary" onclick={() => (logoutConfirmOpen = false)}>{t.common.cancel}</button>
      <button
        type="button"
        class="btn-primary"
        onclick={() => {
          logoutConfirmOpen = false;
          onLogout();
        }}
        data-testid="confirm-logout"
      >
        {t.settings.logout}
      </button>
    </div>
  </div>
</AccessibleDialog>
