<script lang="ts">
  import type { AppSettings, AppSettingsPatch, AppSnapshot, FakeScenario, ThemeMode } from "@ytmcp/protocol";
  import { FAKE_SCENARIO_PRESETS } from "../../fixtures/synthetic.js";
  import { LOCALE_NAMES, LOCALES, t, type Locale } from "../i18n/index.svelte.js";
  import type { CacheClearOutcome, SettingsUpdateOutcome, YtmUiClient } from "../ipc/client.js";
  import AccessibleDialog from "./AccessibleDialog.svelte";
  import Icon from "./Icon.svelte";
  import MomentSettingsSection from "./MomentSettingsSection.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    momentClient?: Pick<YtmUiClient, "momentSettings">;
    momentScopeKey?: string | null;
    momentRevision?: number;
    onUpdateSettings: (patch: AppSettingsPatch) => Promise<SettingsUpdateOutcome>;
    onOpenDiagnostics: () => void;
    onOpenOfficialPlayer?: () => void;
    /** Resolves once core confirms, rejects or fails to confirm the clear. */
    onClearCache: () => Promise<CacheClearOutcome>;
    onLogout: () => void;
    /** A setting failed to save (the latest attempt only); the app shows it as a toast. */
    onSaveFailed?: (message: string) => void;
    /** Mock mode only: the synthetic scenario being shown, how to switch it, and the long test playlist. */
    mockScenario?: {
      readonly active: FakeScenario;
      readonly onSelect: (scenario: FakeScenario) => void;
      readonly onOpenTestPlaylist?: () => void;
    } | null;
    /** Outside the desktop app only (no bridge): switches this window to synthetic data. */
    onUseMockPreview?: (() => void) | null;
  }

  let {
    snapshot,
    momentClient,
    momentScopeKey = null,
    momentRevision = 0,
    onUpdateSettings,
    onOpenDiagnostics,
    onOpenOfficialPlayer,
    onClearCache,
    onLogout,
    onSaveFailed,
    mockScenario = null,
    onUseMockPreview = null,
  }: Props = $props();

  let logoutConfirmOpen = $state(false);
  type CacheClearState = { readonly status: "idle" | "pending" } | CacheClearOutcome;
  let cacheClear = $state<CacheClearState>({ status: "idle" });

  const trayAvailable = $derived(snapshot?.hostHealth.trayAvailable === true);
  const auth = $derived(snapshot?.authState ?? null);

  let pendingValues = $state<Partial<AppSettings>>({});
  const pendingSeq: Partial<Record<keyof AppSettings, number>> = {};
  let saveSeq = 0;
  let saveErrorSeq = 0;

  function settingValue<K extends keyof AppSettings>(key: K): AppSettings[K] {
    if (key in pendingValues) {
      return pendingValues[key] as AppSettings[K];
    }
    return snapshot!.settings[key];
  }

  /** UI scales the host applies; a larger stored value is shown as the largest. */
  const uiScales = [100, 125, 150] as const;
  const shownUiScale = $derived(
    snapshot ? Math.min(150, pendingValues.uiScalePercent ?? snapshot.settings.uiScalePercent ?? 100) : 100
  );

  async function patchSettings(partial: AppSettingsPatch) {
    if (!snapshot) return;
    const seq = ++saveSeq;
    const nextPending: Partial<AppSettings> = { ...pendingValues };
    const entries = Object.entries(partial) as [keyof AppSettings, AppSettings[keyof AppSettings] | undefined][];
    for (const [key, value] of entries) {
      if (value !== undefined) {
        pendingSeq[key] = seq;
        (nextPending as Record<string, unknown>)[key] = value;
      }
    }
    pendingValues = nextPending;

    const outcome = await onUpdateSettings(partial);
    const cleared: Partial<AppSettings> = { ...pendingValues };
    let changed = false;
    for (const [key] of entries) {
      if (pendingSeq[key] === seq) {
        delete pendingSeq[key];
        if (key in cleared) {
          delete cleared[key];
          changed = true;
        }
      }
    }
    if (changed) {
      pendingValues = cleared;
    }

    // Only the latest attempt is reported: an older failure that a newer save overtook is not.
    if (seq >= saveErrorSeq) {
      saveErrorSeq = seq;
      if (outcome.outcome === "failed") onSaveFailed?.(outcome.error.safeMessage);
    }
  }

  async function handleClearCache() {
    if ("status" in cacheClear && cacheClear.status === "pending") return;
    cacheClear = { status: "pending" };
    cacheClear = await onClearCache();
  }
</script>

<section class="browse-page" aria-labelledby="settings-heading" data-testid="view-settings">
  <header class="browse-head page-hero">
    <h1 id="settings-heading" class="browse-title">{t.settings.title}</h1>
  </header>

  <div class="flex flex-col gap-6">
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
              value={settingValue("uiLanguage") ?? "en"}
              onchange={(e) => void patchSettings({ uiLanguage: e.currentTarget.value as Locale })}
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
              value={settingValue("theme")}
              onchange={(e) => void patchSettings({ theme: e.currentTarget.value as ThemeMode })}
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
              checked={settingValue("reduceMotion")}
              onchange={(e) => void patchSettings({ reduceMotion: e.currentTarget.checked })}
              data-testid="setting-reduce-motion"
            />
          </label>
          <label class="setting-row">
            <span>{t.settings.reduceTransparency}</span>
            <input
              type="checkbox"
              role="switch"
              class="toggle"
              checked={settingValue("reduceTransparency")}
              onchange={(e) => void patchSettings({ reduceTransparency: e.currentTarget.checked })}
              data-testid="setting-reduce-transparency"
            />
          </label>
          <label class="setting-row">
            <span>{t.settings.uiScale}</span>
            <select
              class="select tabular-nums"
              value={String(shownUiScale)}
              onchange={(e) => void patchSettings({ uiScalePercent: Number(e.currentTarget.value) })}
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
              checked={settingValue("lrclibEnabled")}
              onchange={(e) => void patchSettings({ lrclibEnabled: e.currentTarget.checked })}
              data-testid="setting-lrclib"
            />
          </label>
          <label class="setting-row">
            <span>{t.settings.autoplayOnLaunch}</span>
            <input
              type="checkbox"
              role="switch"
              class="toggle"
              checked={settingValue("autoplayOnLaunch")}
              onchange={(e) => void patchSettings({ autoplayOnLaunch: e.currentTarget.checked })}
              data-testid="setting-autoplay"
            />
          </label>
          <label class="setting-row">
            <span>{t.settings.trackNotifications}</span>
            <input
              type="checkbox"
              role="switch"
              class="toggle"
              checked={settingValue("trackNotifications")}
              onchange={(e) => void patchSettings({ trackNotifications: e.currentTarget.checked })}
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
                checked={trayAvailable && settingValue("closeToTray")}
                disabled={!trayAvailable}
                aria-describedby={trayAvailable ? undefined : "tray-unavailable-note"}
                onchange={(e) => void patchSettings({ closeToTray: e.currentTarget.checked })}
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

      {#if momentClient}
        <MomentSettingsSection client={momentClient} scopeKey={momentScopeKey} revision={momentRevision} />
      {/if}

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
    {/if}

    <!-- Always present: without a snapshot, Diagnostics still shows why the core is not connected. -->
    <div class="card p-5 flex flex-col gap-4" data-testid="settings-advanced">
      <h2 class="type-lg font-semibold m-0">{t.settings.advancedSection}</h2>
      {#if mockScenario}
        <div class="flex flex-col gap-1.5">
          <label class="setting-row">
            <span>{t.mock.scenarioLabel}</span>
            <select
              class="select"
              value={mockScenario.active}
              onchange={(e) => mockScenario?.onSelect(e.currentTarget.value as FakeScenario)}
              data-testid="mock-scenario-select"
            >
              {#each FAKE_SCENARIO_PRESETS as preset (preset.id)}
                <option value={preset.id}>{preset.label}</option>
              {/each}
            </select>
          </label>
          <p class="type-xs m-0" style="color: var(--text-muted);">{t.mock.description}</p>
          {#if mockScenario.onOpenTestPlaylist}
            <button
              type="button"
              class="btn-secondary self-start"
              onclick={mockScenario.onOpenTestPlaylist}
              data-testid="btn-load-10k-fixture"
            >
              {t.library.load10kFixture}
            </button>
          {/if}
        </div>
      {/if}
      {#if onUseMockPreview}
        <button type="button" class="btn-secondary self-start" onclick={onUseMockPreview} data-testid="btn-enable-mock">
          {t.mock.usePreview}
        </button>
      {/if}
      <div class="flex flex-wrap items-center justify-between gap-4">
        <div role="status" class="type-xs" data-testid="settings-cache-status">
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
            <span style="color: var(--status-danger);" title={cacheClear.error.safeMessage} data-testid="settings-cache-failed">
              {t.settings.cacheClearFailed}
            </span>
          {:else}
            <span style="color: var(--status-warning);" data-testid="settings-cache-unconfirmed">{t.settings.cacheClearUnconfirmed}</span>
          {/if}
        </div>
        <div class="flex flex-wrap gap-2">
          <button type="button" class="btn-primary" onclick={onOpenDiagnostics} data-testid="settings-open-diagnostics">
            {t.settings.openDiagnosticsDialog}
          </button>
          <button
            type="button"
            class="btn-secondary"
            disabled={!snapshot || ("status" in cacheClear && cacheClear.status === "pending")}
            aria-busy={"status" in cacheClear && cacheClear.status === "pending"}
            onclick={() => void handleClearCache()}
            data-testid="settings-clear-cache"
          >
            {t.settings.clearCache}
          </button>
        </div>
      </div>
    </div>
  </div>
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
