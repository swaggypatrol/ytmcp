import { describe, expect, it, vi } from "vitest";
import { MAX_FRAME_BYTES, PROTOCOL_VERSION, type SessionMaterialPayload } from "@ytmcp/protocol";

const mockSafeStorageState = {
  encryptionAvailable: true,
  backend: "basic_text" as "basic_text" | "kwallet6" | "gnome_libsecret",
};

vi.mock("electron", () => {
  return {
    app: {
      isReady: () => false,
      requestSingleInstanceLock: () => true,
      setName: vi.fn(),
      setDesktopName: vi.fn(),
      whenReady: async () => undefined,
      on: vi.fn(),
      quit: vi.fn(),
      exit: vi.fn(),
    },
    protocol: {
      registerSchemesAsPrivileged: vi.fn(),
    },
    ipcMain: {
      handle: vi.fn(),
    },
    net: {
      fetch: vi.fn(),
    },
    session: {
      fromPartition: vi.fn(),
    },
    BrowserWindow: vi.fn(),
    safeStorage: {
      isEncryptionAvailable: () => mockSafeStorageState.encryptionAvailable,
      getSelectedStorageBackend: () => mockSafeStorageState.backend,
    },
  };
});

import { loadAppIdentity, parseAppIdentity } from "../../bootstrap/identity.js";
import { validateElectronIpcEvent, validateRendererIpc } from "../../bootstrap/ipc-guard.js";
import {
  SessionCredentialBroker,
  buildCookiePairs,
  querySafeStorageStatus,
  redactSessionMaterial,
} from "../../session/credential-broker.js";
import {
  MUSIC_SIGN_IN_URL,
  buildMainWindowOptions,
  buildPlayerWindowOptions,
  isAllowedMainNavigationUrl,
  isAllowedPlayerNavigationUrl,
  isBridgeOrigin,
} from "../../windows/window-manager.js";

describe("app-identity & window security options", () => {
  it("loads config/app-identity.json with isolated partitions", () => {
    const identity = loadAppIdentity();
    expect(identity.appId).toBe("local.ytmcp.Desktop");
    expect(identity.playerSessionPartition).toBe("persist:player-default");
    expect(identity.uiSessionPartition).toBe("ytm-ui-ephemeral");
    expect(identity.appScheme).toBe("app");
  });

  it("rejects identity where player and ui share partition", () => {
    const valid = loadAppIdentity();
    expect(() =>
      parseAppIdentity({
        ...valid,
        uiSessionPartition: valid.playerSessionPartition,
      })
    ).toThrow(/must differ/);
  });

  it("sets sandbox, contextIsolation, webSecurity, and restricts autoplayPolicy to player only", () => {
    const identity = loadAppIdentity();
    const mainOpts = buildMainWindowOptions(identity, "/tmp/ui-preload.cjs");
    const playerOpts = buildPlayerWindowOptions(identity, "/tmp/player-preload.cjs");

    expect(mainOpts.frame).toBe(true);
    expect(mainOpts.webPreferences?.sandbox).toBe(true);
    expect(mainOpts.webPreferences?.contextIsolation).toBe(true);
    expect(mainOpts.webPreferences?.nodeIntegration).toBe(false);
    expect(mainOpts.webPreferences?.webSecurity).toBe(true);
    expect(mainOpts.webPreferences?.partition).toBe(identity.uiSessionPartition);
    expect(mainOpts.webPreferences?.autoplayPolicy).toBeUndefined();

    expect(playerOpts.show).toBe(false);
    expect(playerOpts.paintWhenInitiallyHidden).toBe(false);
    expect(playerOpts.webPreferences?.sandbox).toBe(true);
    expect(playerOpts.webPreferences?.contextIsolation).toBe(true);
    expect(playerOpts.webPreferences?.nodeIntegration).toBe(false);
    expect(playerOpts.webPreferences?.webSecurity).toBe(true);
    expect(playerOpts.webPreferences?.partition).toBe(identity.playerSessionPartition);
    expect(playerOpts.webPreferences?.autoplayPolicy).toBe("no-user-gesture-required");
  });
});

describe("navigation & origin allowlists", () => {
  it("allows only music.youtube.com and the Google sign-in redirect chain in player window", () => {
    expect(isAllowedPlayerNavigationUrl("https://music.youtube.com/")).toBe(true);
    expect(isAllowedPlayerNavigationUrl("https://music.youtube.com/watch?v=abc")).toBe(true);
    expect(isAllowedPlayerNavigationUrl("https://accounts.google.com/ServiceLogin")).toBe(true);
    expect(isAllowedPlayerNavigationUrl("https://accounts.youtube.com/accounts/SetSID")).toBe(true);
    expect(isAllowedPlayerNavigationUrl("https://consent.youtube.com/ml")).toBe(true);
    expect(isAllowedPlayerNavigationUrl("https://consent.google.com/ml")).toBe(true);
    expect(
      isAllowedPlayerNavigationUrl(
        "https://www.youtube.com/signin?action_handle_signin=true&next=https%3A%2F%2Fmusic.youtube.com%2F"
      )
    ).toBe(true);
    expect(isAllowedPlayerNavigationUrl("https://gds.google.com/web/chip")).toBe(true);
    expect(isAllowedPlayerNavigationUrl(MUSIC_SIGN_IN_URL)).toBe(true);

    // Sign-in helper hosts are allowed only on their redirect paths, not as general browsing.
    expect(isAllowedPlayerNavigationUrl("https://www.youtube.com/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://www.youtube.com/watch?v=abc")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://youtube.com/signin")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://www.google.com/search?q=x")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://accounts.youtube.com/other")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://myaccount.google.com/")).toBe(false);
    expect(isBridgeOrigin("https://www.youtube.com/signin")).toBe(false);
    expect(isBridgeOrigin("https://accounts.google.com/ServiceLogin")).toBe(false);

    // Reject insecure schemes, spoofs, ports, credentials, and unrelated hosts
    expect(isAllowedPlayerNavigationUrl("http://music.youtube.com/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://music.youtube.com:8443/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://user:pass@music.youtube.com/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://music.youtube.com.evil.example/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://evil.accounts.google.com/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("https://example.com/")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("file:///etc/passwd")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("javascript:alert(1)")).toBe(false);
    expect(isAllowedPlayerNavigationUrl("app://ui/")).toBe(false);
  });

  it("enables bridge origin strictly for https://music.youtube.com", () => {
    expect(isBridgeOrigin("https://music.youtube.com/")).toBe(true);
    expect(isBridgeOrigin("https://music.youtube.com/library")).toBe(true);
    expect(isBridgeOrigin("https://accounts.google.com/")).toBe(false);
    expect(isBridgeOrigin("http://music.youtube.com/")).toBe(false);
    expect(isBridgeOrigin("app://ui/")).toBe(false);
  });

  it("restricts main window navigation to app://ui and explicit devUiUrl", () => {
    expect(isAllowedMainNavigationUrl("app://ui/index.html", "app")).toBe(true);
    expect(isAllowedMainNavigationUrl("app://covers/abc", "app")).toBe(false);
    expect(isAllowedMainNavigationUrl("https://music.youtube.com/", "app")).toBe(false);
    expect(isAllowedMainNavigationUrl("file:///tmp/x.html", "app")).toBe(false);
    expect(
      isAllowedMainNavigationUrl("http://localhost:5173/now-playing", "app", "http://localhost:5173")
    ).toBe(true);
    expect(
      isAllowedMainNavigationUrl("http://localhost:9999/", "app", "http://localhost:5173")
    ).toBe(false);
  });
});

describe("IPC sender, frame, origin, size, and role guards", () => {
  const validUiEnvelope = {
    protocolVersion: PROTOCOL_VERSION,
    kind: "ui.ping",
    seq: 1,
    payload: {},
  };

  const validBridgeEnvelope = {
    protocolVersion: PROTOCOL_VERSION,
    kind: "bridge.health",
    bridgeEpoch: "bridge-1",
    seq: 1,
    payload: {
      bridgeEpoch: "bridge-1",
      controlsBound: true,
      healthy: true,
      videoElementPresent: true,
    },
  };

  it("accepts valid top-level mainUi and playerBridge envelopes", () => {
    const uiRes = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      validUiEnvelope
    );
    expect(uiRes.ok).toBe(true);

    const bridgeRes = validateRendererIpc(
      {
        role: "playerBridge",
        allowedOrigins: new Set(["https://music.youtube.com"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "https://music.youtube.com" },
        bridgeOriginActive: true,
      },
      validBridgeEnvelope
    );
    expect(bridgeRes.ok).toBe(true);
  });

  it("rejects mismatched webContents, subframes, wrong origins, and inactive bridge origins", () => {
    const wrongSender = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: false,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      validUiEnvelope
    );
    expect(wrongSender.ok).toBe(false);
    if (!wrongSender.ok) expect(wrongSender.reason).toBe("unexpected_web_contents");

    const subframe = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: false, origin: "app://ui" },
      },
      validUiEnvelope
    );
    expect(subframe.ok).toBe(false);
    if (!subframe.ok) expect(subframe.reason).toBe("subframe_rejected");

    const wrongOrigin = validateRendererIpc(
      {
        role: "playerBridge",
        allowedOrigins: new Set(["https://music.youtube.com"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "https://accounts.google.com" },
        bridgeOriginActive: true,
      },
      validBridgeEnvelope
    );
    expect(wrongOrigin.ok).toBe(false);
    if (!wrongOrigin.ok) expect(wrongOrigin.reason).toBe("disallowed_origin");

    const inactiveBridge = validateRendererIpc(
      {
        role: "playerBridge",
        allowedOrigins: new Set(["https://music.youtube.com"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "https://music.youtube.com" },
        bridgeOriginActive: false,
      },
      validBridgeEnvelope
    );
    expect(inactiveBridge.ok).toBe(false);
    if (!inactiveBridge.ok) expect(inactiveBridge.reason).toBe("bridge_inactive_for_origin");
  });

  it("rejects cross-role messages, secret sessionMaterial, oversized payloads, and invalid schemas", () => {
    const crossRoleUiToBridge = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      validBridgeEnvelope
    );
    expect(crossRoleUiToBridge.ok).toBe(false);
    if (!crossRoleUiToBridge.ok) expect(crossRoleUiToBridge.reason).toBe("role_forbidden");

    const secretFromUi = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      {
        protocolVersion: PROTOCOL_VERSION,
        kind: "host.sessionMaterial",
        seq: 1,
        payload: {
          accountEpoch: "a1",
          credentialRevision: "c1",
          cookiePairs: { SAPISID: "secret" },
        },
      }
    );
    expect(secretFromUi.ok).toBe(false);
    if (!secretFromUi.ok) expect(secretFromUi.reason).toBe("role_forbidden");

    const crossRoleBridgeToUi = validateRendererIpc(
      {
        role: "playerBridge",
        allowedOrigins: new Set(["https://music.youtube.com"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "https://music.youtube.com" },
        bridgeOriginActive: true,
      },
      validUiEnvelope
    );
    expect(crossRoleBridgeToUi.ok).toBe(false);
    if (!crossRoleBridgeToUi.ok) expect(crossRoleBridgeToUi.reason).toBe("role_forbidden");

    const oversized = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      {
        protocolVersion: PROTOCOL_VERSION,
        kind: "ui.catalogQuery",
        seq: 1,
        payload: { kind: "searchSongs", query: "x".repeat(MAX_FRAME_BYTES + 128) },
      }
    );
    expect(oversized.ok).toBe(false);
    if (!oversized.ok) expect(oversized.reason).toBe("payload_too_large");

    const badSchema = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      { protocolVersion: 99, kind: "ui.ping", seq: 1, payload: {} }
    );
    expect(badSchema.ok).toBe(false);
    if (!badSchema.ok) expect(badSchema.code).toBe("BRIDGE_INCOMPATIBLE");
  });

  it("validates Electron event sender and mainFrame object identity", () => {
    const mainFrame = { origin: "app://ui", url: "app://ui/", parent: null } as never;
    const subframe = { origin: "app://ui", url: "app://ui/sub", parent: mainFrame } as never;
    const expectedWc = {
      isDestroyed: () => false,
      mainFrame,
    } as never;

    const okRes = validateElectronIpcEvent(
      { sender: expectedWc, senderFrame: mainFrame },
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        getWebContents: () => expectedWc,
      },
      validUiEnvelope
    );
    expect(okRes.ok).toBe(true);

    const subRes = validateElectronIpcEvent(
      { sender: expectedWc, senderFrame: subframe },
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        getWebContents: () => expectedWc,
      },
      validUiEnvelope
    );
    expect(subRes.ok).toBe(false);
    if (!subRes.ok) expect(subRes.reason).toBe("subframe_rejected");
  });
});

describe("SessionCredentialBroker & cookie redaction", () => {
  it("redacts all cookie values and names from session summaries", () => {
    const payload: SessionMaterialPayload = {
      accountEpoch: "acct-1",
      credentialRevision: "cred-3",
      authUserIndex: null,
      channelLabel: null,
      cookiePairs: buildCookiePairs([
        { name: "SAPISID", value: "top-secret-sapisid-value" },
        { name: "__Secure-3PSID", value: "top-secret-3psid-value" },
      ]),
    };

    const summary = redactSessionMaterial(payload);
    const serialized = JSON.stringify(summary);

    expect(summary.redacted).toBe(true);
    expect(summary.cookieCount).toBe(2);
    expect(summary.hasAuthCookies).toBe(true);
    expect(serialized).not.toContain("top-secret-sapisid-value");
    expect(serialized).not.toContain("top-secret-3psid-value");
    expect(serialized).not.toContain("SAPISID");
  });

  it("reports Linux basic_text honestly as non-secure storage", () => {
    mockSafeStorageState.encryptionAvailable = true;
    mockSafeStorageState.backend = "basic_text";
    const basicStatus = querySafeStorageStatus();
    expect(basicStatus.electronEncryptionAvailable).toBe(true);
    expect(basicStatus.selectedBackend).toBe("basic_text");
    expect(basicStatus.basicTextWarning).toBe(true);
    expect(basicStatus.secureStorageActive).toBe(false);

    mockSafeStorageState.backend = "kwallet6";
    const kwalletStatus = querySafeStorageStatus();
    expect(kwalletStatus.basicTextWarning).toBe(false);
    expect(kwalletStatus.secureStorageActive).toBe(true);
  });

  it("tracks credentialRevision monotonically on relevant cookie changes and clears on logout", async () => {
    const listeners = new Set<(...args: unknown[]) => void>();
    let currentCookies: Array<{ name: string; value: string; domain: string }> = [
      { name: "SAPISID", value: "synthetic-cookie-1", domain: ".youtube.com" },
    ];

    const fakeSession = {
      cookies: {
        get: vi.fn(async (filter: { url?: string }) => {
          expect(filter.url).toBe("https://music.youtube.com");
          return currentCookies;
        }),
        on: vi.fn((_event: string, fn: (...args: unknown[]) => void) => {
          listeners.add(fn);
        }),
        removeListener: vi.fn((_event: string, fn: (...args: unknown[]) => void) => {
          listeners.delete(fn);
        }),
      },
    };

    const receivedMaterials: SessionMaterialPayload[] = [];
    const logs: unknown[] = [];
    const clearedReasons: string[] = [];

    const broker = new SessionCredentialBroker({
      playerSession: fakeSession as never,
      debounceMs: 0,
      onSessionMaterial: (p) => receivedMaterials.push(p),
      onSessionCleared: (r) => clearedReasons.push(r),
      onSummaryLog: (s) => logs.push(s),
    });

    broker.start();
    await new Promise((r) => setTimeout(r, 10));
    expect(receivedMaterials).toHaveLength(1);
    expect(receivedMaterials[0]?.credentialRevision).toBe("cred-1");
    const initialEpoch = receivedMaterials[0]?.accountEpoch;

    // Unrelated domain should NOT bump credentialRevision
    for (const fn of listeners) {
      fn({}, { domain: "example.com" }, "explicit", false);
    }
    await new Promise((r) => setTimeout(r, 10));
    expect(receivedMaterials).toHaveLength(1);

    // Identical cookie set on .youtube.com must NOT bump credentialRevision or re-emit
    for (const fn of listeners) {
      fn({}, { domain: ".youtube.com" }, "explicit", false);
    }
    await new Promise((r) => setTimeout(r, 10));
    expect(receivedMaterials).toHaveLength(1);

    // Rotation of a non-sign-in cookie advances credentialRevision while preserving accountEpoch
    currentCookies = [
      { name: "SAPISID", value: "synthetic-cookie-1", domain: ".youtube.com" },
      { name: "SIDCC", value: "synthetic-rotating-2", domain: ".youtube.com" },
    ];
    for (const fn of listeners) {
      fn({}, { domain: ".youtube.com" }, "explicit", false);
    }
    await new Promise((r) => setTimeout(r, 10));
    expect(receivedMaterials).toHaveLength(2);
    expect(receivedMaterials[1]?.credentialRevision).toBe("cred-2");
    expect(receivedMaterials[1]?.accountEpoch).toBe(initialEpoch);

    // Different sign-in cookies with no sign-out read in between: another account, new epoch
    currentCookies = [{ name: "SAPISID", value: "synthetic-cookie-2", domain: ".youtube.com" }];
    for (const fn of listeners) {
      fn({}, { domain: ".youtube.com" }, "explicit", false);
    }
    await new Promise((r) => setTimeout(r, 10));
    expect(receivedMaterials).toHaveLength(3);
    expect(receivedMaterials[2]?.credentialRevision).toBe("cred-3");
    expect(receivedMaterials[2]?.accountEpoch).not.toBe(initialEpoch);
    const switchedEpoch = receivedMaterials[2]?.accountEpoch;

    // Clearing auth cookies emits session cleared and rotates accountEpoch
    currentCookies = [];
    await broker.refreshNow();
    expect(clearedReasons).toEqual(["auth_cookies_removed"]);
    expect(broker.getAccountEpoch()).not.toBe(initialEpoch);
    expect(broker.getAccountEpoch()).not.toBe(switchedEpoch);

    // Verify summary logs contain zero raw cookie values
    const logDump = JSON.stringify(logs);
    expect(logDump).not.toContain("synthetic-cookie-1");
    expect(logDump).not.toContain("synthetic-cookie-2");

    broker.stop();
  });

  it("delivers only the latest overlapping cookie read and preserves pending resends", async () => {
    // The sign-in cookie stays; a rotating cookie changes, as in a same-account refresh.
    const cookies = (value: string) => [{ name: "SAPISID", value: "synthetic-sign-in" }, { name: "SIDCC", value }];
    const reads: Array<(value: ReturnType<typeof cookies>) => void> = [];
    const fakeSession = {
      cookies: {
        get: () => new Promise<ReturnType<typeof cookies>>((resolve) => reads.push(resolve)),
        on: vi.fn(),
        removeListener: vi.fn(),
      },
    };
    const received: SessionMaterialPayload[] = [];
    const broker = new SessionCredentialBroker({
      playerSession: fakeSession as never,
      onSessionMaterial: (material) => received.push(material),
    });
    broker.start();
    reads[0]!(cookies("initial"));
    await Promise.resolve();

    // A cookie refresh supersedes a slow re-send after network recovery or a core restart.
    const first = broker.resendCurrent();
    const second = broker.refreshNow();
    reads[2]!(cookies("rotated"));
    expect((await second)?.credentialRevision).toBe("cred-2");
    reads[1]!(cookies("initial"));
    expect(await first).toBeNull();
    expect(broker.getCredentialRevision()).toBe("cred-2");

    // A restart still needs material even if the superseding refresh finds unchanged cookies.
    const resend = broker.resendCurrent();
    const refresh = broker.refreshNow();
    reads[4]!(cookies("rotated"));
    expect((await refresh)?.credentialRevision).toBe("cred-2");
    reads[3]!([]);
    expect(await resend).toBeNull();
    expect(received.map((entry) => entry.cookiePairs.SIDCC)).toEqual(["initial", "rotated", "rotated"]);
    expect(new Set(received.map((entry) => entry.accountEpoch)).size).toBe(1);
    expect(broker.getAccountEpoch()).toBe(received[0]?.accountEpoch);
    broker.stop();
  });

  it("drops a cookie read that began before a stop/reset/start logout cycle", async () => {
    const reads: Array<(cookies: unknown[]) => void> = [];
    const fakeSession = {
      cookies: {
        get: vi.fn(() => new Promise<unknown[]>((resolveRead) => reads.push(resolveRead))),
        on: vi.fn(),
        removeListener: vi.fn(),
      },
    };
    const received: SessionMaterialPayload[] = [];
    const broker = new SessionCredentialBroker({
      playerSession: fakeSession as never,
      onSessionMaterial: (p) => received.push(p),
    });
    const preLogout = [{ name: "SAPISID", value: "old-account", domain: ".youtube.com" }];
    const postLogin = [{ name: "SAPISID", value: "new-account", domain: ".youtube.com" }];

    broker.start();
    broker.stop();
    broker.reset();
    broker.start();
    expect(reads).toHaveLength(2);

    // The pre-logout read resolves after the restart: it must not be sent under the new epoch.
    reads[0]!(preLogout);
    reads[1]!(postLogin);
    await new Promise((r) => setTimeout(r, 0));

    expect(received.map((p) => p.cookiePairs.SAPISID)).toEqual(["new-account"]);
    broker.stop();
  });

  it("re-sends unchanged material once per retriable error with bounded backoff, and on network recovery", async () => {
    vi.useFakeTimers();
    try {
      const fakeSession = {
        cookies: {
          get: vi.fn(async () => [{ name: "SAPISID", value: "synthetic-cookie", domain: ".youtube.com" }]),
          on: vi.fn(),
          removeListener: vi.fn(),
        },
      };
      const received: SessionMaterialPayload[] = [];
      const broker = new SessionCredentialBroker({
        playerSession: fakeSession as never,
        onSessionMaterial: (p) => received.push(p),
        resendBackoffMs: [1_000, 5_000],
      });
      const offline = { code: "NETWORK_OFFLINE", retriable: true } as const;
      broker.start();
      await vi.advanceTimersByTimeAsync(0);
      expect(received).toHaveLength(1);

      // Non-retriable errors (stale, invalid, auth) never trigger a re-send.
      broker.handleDeliveryError({ retriable: false });
      await vi.advanceTimersByTimeAsync(60_000);
      expect(received).toHaveLength(1);

      // One re-send per error after the backoff delay; duplicate errors while one is pending add nothing.
      broker.handleDeliveryError(offline);
      broker.handleDeliveryError(offline);
      await vi.advanceTimersByTimeAsync(999);
      expect(received).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(1);
      expect(received).toHaveLength(2);
      broker.handleDeliveryError(offline);
      await vi.advanceTimersByTimeAsync(5_000);
      expect(received).toHaveLength(3);

      // Budget exhausted: further errors wait for recovery instead of looping.
      broker.handleDeliveryError(offline);
      await vi.advanceTimersByTimeAsync(600_000);
      expect(received).toHaveLength(3);

      // Network recovery re-sends immediately and restores the budget.
      broker.handleNetworkOnline();
      await vi.advanceTimersByTimeAsync(0);
      expect(received).toHaveLength(4);
      broker.handleDeliveryError(offline);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(received).toHaveLength(5);

      // Re-sends carry the same account and revision: the material did not change.
      expect(new Set(received.map((p) => p.accountEpoch)).size).toBe(1);
      expect(new Set(received.map((p) => p.credentialRevision))).toEqual(new Set(["cred-1"]));

      // Stopping (logout) cancels a pending re-send.
      broker.handleDeliveryError(offline);
      broker.stop();
      await vi.advanceTimersByTimeAsync(600_000);
      expect(received).toHaveLength(5);
      broker.handleNetworkOnline();
      await vi.advanceTimersByTimeAsync(0);
      expect(received).toHaveLength(5);
    } finally {
      vi.useRealTimers();
    }
  });
});
