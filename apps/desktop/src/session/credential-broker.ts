import { createHash, randomUUID } from "node:crypto";
import type { QueryError, SessionMaterialPayload } from "@ytmcp/protocol";
import { safeStorage, type Cookie, type CookiesGetFilter, type Event, type Session } from "electron";
import { MUSIC_ORIGIN } from "../../preload/channels.js";

export type SafeStorageBackend =
  | "basic_text"
  | "gnome_libsecret"
  | "kwallet"
  | "kwallet5"
  | "kwallet6"
  | "unknown"
  | "unavailable";

export interface SafeStorageStatus {
  readonly electronEncryptionAvailable: boolean;
  readonly selectedBackend: SafeStorageBackend;
  readonly secureStorageActive: boolean;
  readonly basicTextWarning: boolean;
}

export interface RedactedSessionSummary {
  readonly accountEpoch: string;
  readonly credentialRevision: string;
  readonly cookieCount: number;
  readonly hasAuthCookies: boolean;
  readonly redacted: true;
}

const AUTH_COOKIE_NAMES: ReadonlySet<string> = new Set([
  "SAPISID",
  "__Secure-3PAPISID",
  "__Secure-1PAPISID",
  "SID",
  "__Secure-3PSID",
  "__Secure-1PSID",
]);

const RELEVANT_COOKIE_DOMAIN_SUFFIXES = [
  "music.youtube.com",
  "youtube.com",
  "google.com",
];

function isRelevantCookieDomain(domain: string): boolean {
  const normalized = domain.replace(/^\./, "").toLowerCase();
  return RELEVANT_COOKIE_DOMAIN_SUFFIXES.some(
    (suffix) => normalized === suffix || normalized.endsWith(`.${suffix}`)
  );
}

/**
 * Converts Electron Cookie objects into a flat name -> value map.
 * Never logs or writes values to disk.
 */
export function buildCookiePairs(
  cookies: ReadonlyArray<Pick<Cookie, "name" | "value">>
): Record<string, string> {
  const pairs: Record<string, string> = Object.create(null) as Record<string, string>;
  for (const cookie of cookies) {
    if (cookie.name && typeof cookie.value === "string") {
      pairs[cookie.name] = cookie.value;
    }
  }
  return pairs;
}

export function hasAuthCookies(cookiePairs: Readonly<Record<string, string | undefined>>): boolean {
  for (const name of AUTH_COOKIE_NAMES) {
    const val = cookiePairs[name];
    if (typeof val === "string" && val.length > 0) {
      return true;
    }
  }
  return false;
}

/**
 * Produces a strictly redacted summary of a SessionMaterialPayload safe for
 * diagnostic logs and test assertions. Strips all cookie values and names.
 */
export function redactSessionMaterial(payload: SessionMaterialPayload): RedactedSessionSummary {
  const entries = Object.entries(payload.cookiePairs).filter(
    ([, val]) => typeof val === "string" && val.length > 0
  );
  return {
    accountEpoch: payload.accountEpoch,
    credentialRevision: payload.credentialRevision,
    cookieCount: entries.length,
    hasAuthCookies: hasAuthCookies(payload.cookiePairs),
    redacted: true,
  };
}

export function querySafeStorageStatus(): SafeStorageStatus {
  let electronEncryptionAvailable = false;
  let selectedBackend: SafeStorageBackend = "unavailable";

  try {
    electronEncryptionAvailable = safeStorage.isEncryptionAvailable();
    if (typeof safeStorage.getSelectedStorageBackend === "function") {
      selectedBackend = safeStorage.getSelectedStorageBackend();
    } else {
      selectedBackend = electronEncryptionAvailable ? "unknown" : "unavailable";
    }
  } catch {
    electronEncryptionAvailable = false;
    selectedBackend = "unavailable";
  }

  const basicTextWarning = selectedBackend === "basic_text";
  const secureStorageActive =
    electronEncryptionAvailable &&
    !basicTextWarning &&
    selectedBackend !== "unavailable";

  return {
    electronEncryptionAvailable,
    selectedBackend,
    secureStorageActive,
    basicTextWarning,
  };
}

/**
 * Delays before re-sending the current material after a retriable delivery error, one
 * attempt per entry. After the last one the broker waits for network recovery, new
 * cookies or a core restart instead of retrying forever.
 */
export const SESSION_RESEND_BACKOFF_MS: readonly number[] = [2_000, 10_000, 30_000, 120_000, 300_000];

export interface SessionCredentialBrokerOptions {
  readonly playerSession: Pick<Session, "cookies">;
  readonly onSessionMaterial: (payload: SessionMaterialPayload) => void;
  readonly onSessionCleared?: (reason: string) => void;
  readonly onSummaryLog?: (summary: RedactedSessionSummary | { cleared: true; reason: string }) => void;
  readonly debounceMs?: number;
  readonly musicUrl?: string;
  readonly resendBackoffMs?: readonly number[];
}

/**
 * Host-only credential broker reading cookies exclusively from the isolated
 * `persist:player-default` session and forwarding redacted-from-logs
 * `SessionMaterialPayload` solely to the Rust core callback.
 */
export class SessionCredentialBroker {
  private readonly playerSession: Pick<Session, "cookies">;
  private readonly onSessionMaterial: (payload: SessionMaterialPayload) => void;
  private readonly onSessionCleared?: (reason: string) => void;
  private readonly onSummaryLog?: (
    summary: RedactedSessionSummary | { cleared: true; reason: string }
  ) => void;
  private readonly debounceMs: number;
  private readonly musicUrl: string;
  private readonly resendBackoffMs: readonly number[];

  private started = false;
  /** A newer read, stop or reset supersedes every older cookie snapshot. */
  private readSeq = 0;
  private resendPending = false;
  private accountEpoch: string = `acct-${randomUUID()}`;
  private revisionCounter = 0;
  private lastAuthenticated = false;
  private lastCookieDigest: string | null = null;
  private lastLoginDigest: string | null = null;
  private debounceTimer: NodeJS.Timeout | null = null;
  private resendTimer: NodeJS.Timeout | null = null;
  /** Retriable delivery errors answered with a re-send since the last fresh material. */
  private resendAttempts = 0;
  private cookieListener:
    | ((event: Event, cookie: Cookie, cause: string, removed: boolean) => void)
    | null = null;

  public constructor(options: SessionCredentialBrokerOptions) {
    this.playerSession = options.playerSession;
    this.onSessionMaterial = options.onSessionMaterial;
    this.onSessionCleared = options.onSessionCleared;
    this.onSummaryLog = options.onSummaryLog;
    this.debounceMs = options.debounceMs ?? 250;
    this.musicUrl = options.musicUrl ?? MUSIC_ORIGIN;
    this.resendBackoffMs = options.resendBackoffMs ?? SESSION_RESEND_BACKOFF_MS;
  }

  public getAccountEpoch(): string {
    return this.accountEpoch;
  }

  public getCredentialRevision(): string {
    return `cred-${this.revisionCounter}`;
  }

  public start(): void {
    if (this.started) {
      return;
    }
    this.started = true;

    this.cookieListener = (_event, cookie, _cause, _removed) => {
      if (!isRelevantCookieDomain(cookie.domain ?? "")) {
        return;
      }
      this.scheduleRefresh();
    };
    this.playerSession.cookies.on("changed", this.cookieListener);
    void this.refreshNow();
  }

  public stop(): void {
    if (!this.started) {
      return;
    }
    this.started = false;
    this.readSeq += 1;
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = null;
    }
    this.clearResend();
    if (this.cookieListener) {
      this.playerSession.cookies.removeListener("changed", this.cookieListener);
      this.cookieListener = null;
    }
  }

  /**
   * Forgets the current session after an explicit logout (call while stopped): the next
   * authenticated cookie set is treated as a new sign-in with a fresh accountEpoch.
   */
  public reset(): void {
    this.readSeq += 1;
    this.clearResend();
    this.lastAuthenticated = false;
    this.lastCookieDigest = null;
    this.lastLoginDigest = null;
    this.rotateAccountEpoch();
  }

  public rotateAccountEpoch(): string {
    this.accountEpoch = `acct-${randomUUID()}`;
    return this.accountEpoch;
  }

  /**
   * The core could not apply the last material (e.g. its identity read failed with
   * NETWORK_OFFLINE or TIMEOUT). A retriable error re-sends the current material once after
   * the next backoff delay; at most one re-send is pending, and the attempts run out after
   * `resendBackoffMs`. Non-retriable errors (stale, invalid, auth expired) are not retried.
   */
  public handleDeliveryError(error: Pick<QueryError, "retriable">): void {
    if (!this.started || !error.retriable || this.resendTimer) {
      return;
    }
    const delay = this.resendBackoffMs[this.resendAttempts];
    if (delay === undefined) {
      return;
    }
    this.resendAttempts += 1;
    this.resendTimer = setTimeout(() => {
      this.resendTimer = null;
      void this.resendCurrent();
    }, delay);
  }

  /** Network came back (or the machine woke up): re-send the current material once and restart the backoff. */
  public handleNetworkOnline(): void {
    if (!this.started || !this.lastAuthenticated) {
      return;
    }
    this.clearResend();
    void this.resendCurrent();
  }

  /**
   * Re-reads the cookies and sends them even when unchanged (same accountEpoch and
   * credentialRevision), e.g. after a failed delivery or a core restart that lost them.
   */
  public resendCurrent(): Promise<RedactedSessionSummary | null> {
    return this.read(true);
  }

  private clearResend(): void {
    if (this.resendTimer) {
      clearTimeout(this.resendTimer);
      this.resendTimer = null;
    }
    this.resendAttempts = 0;
    this.resendPending = false;
  }

  private scheduleRefresh(): void {
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
    }
    if (this.debounceMs <= 0) {
      void this.refreshNow();
      return;
    }
    this.debounceTimer = setTimeout(() => {
      this.debounceTimer = null;
      void this.refreshNow();
    }, this.debounceMs);
  }

  /** Digest of the sign-in cookies: stable for one Google sign-in, different for another. */
  private computeLoginDigest(cookiePairs: Readonly<Record<string, string>>): string {
    const hash = createHash("sha256");
    for (const name of [...AUTH_COOKIE_NAMES].sort()) {
      hash.update(name);
      hash.update("=");
      hash.update(cookiePairs[name] ?? "");
      hash.update("\0");
    }
    return hash.digest("hex");
  }

  private computeCookieDigest(cookiePairs: Readonly<Record<string, string>>): string {
    const hash = createHash("sha256");
    const entries = Object.entries(cookiePairs).sort(([a], [b]) => a.localeCompare(b));
    for (const [name, value] of entries) {
      hash.update(name);
      hash.update("=");
      hash.update(value);
      hash.update("\0");
    }
    return hash.digest("hex");
  }

  public refreshNow(): Promise<RedactedSessionSummary | null> {
    return this.read(false);
  }

  private async read(resend: boolean): Promise<RedactedSessionSummary | null> {
    if (!this.started) return null;
    const filter: CookiesGetFilter = { url: this.musicUrl };
    const seq = ++this.readSeq;
    this.resendPending ||= resend;
    const cookies = await this.playerSession.cookies.get(filter);
    if (seq !== this.readSeq) {
      return null;
    }
    // A superseding refresh must honor a re-send requested by a restarted core.
    const shouldResend = this.resendPending;
    this.resendPending = false;
    const cookiePairs = buildCookiePairs(cookies);
    const authenticated = hasAuthCookies(cookiePairs);

    if (!authenticated) {
      if (this.lastAuthenticated) {
        this.lastAuthenticated = false;
        this.lastCookieDigest = null;
        this.lastLoginDigest = null;
        this.rotateAccountEpoch();
        this.onSessionCleared?.("auth_cookies_removed");
        this.onSummaryLog?.({ cleared: true, reason: "auth_cookies_removed" });
      }
      return null;
    }

    // Another sign-in seen without a sign-out in between (both reads authenticated) is a new
    // account epoch, never a credential refresh of the previous account.
    const loginDigest = this.computeLoginDigest(cookiePairs);
    if (this.lastAuthenticated && this.lastLoginDigest !== null && this.lastLoginDigest !== loginDigest) {
      this.rotateAccountEpoch();
    }
    this.lastLoginDigest = loginDigest;

    const cookieDigest = this.computeCookieDigest(cookiePairs);
    const unchanged = this.lastAuthenticated && this.lastCookieDigest === cookieDigest;
    if (unchanged && !shouldResend) {
      return null;
    }

    if (!unchanged) {
      if (!this.lastAuthenticated) {
        this.rotateAccountEpoch();
        this.lastAuthenticated = true;
      }
      this.lastCookieDigest = cookieDigest;
      this.revisionCounter += 1;
      if (!resend) {
        // Fresh material from a cookie change starts a new retry budget.
        this.resendAttempts = 0;
      }
    }
    const payload: SessionMaterialPayload = {
      accountEpoch: this.accountEpoch,
      authUserIndex: null,
      channelLabel: null,
      cookiePairs,
      credentialRevision: this.getCredentialRevision(),
    };

    const summary = redactSessionMaterial(payload);
    this.onSummaryLog?.(summary);
    this.onSessionMaterial(payload);
    return summary;
  }
}
