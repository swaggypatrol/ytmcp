import type { AuthState, CapabilitySupport, NetworkState, QueryError } from "@ytm-glass/protocol";

export type PageStatus = "idle" | "loading" | "done";

/** The SPEC 8.2 page states; `ready` pages may additionally be `cached`. */
export type PageKind = "loading" | "empty" | "offline" | "authExpired" | "unsupported" | "error" | "ready";

export interface PageView {
  readonly kind: PageKind;
  /** Content comes from a stale cache or the network is offline. */
  readonly cached: boolean;
  readonly error: QueryError | null;
}

export interface PageViewInput {
  readonly status: PageStatus;
  readonly error: QueryError | null;
  /** The page holds content from an earlier successful response. */
  readonly hasData: boolean;
  readonly isEmpty: boolean;
  readonly stale: boolean;
  readonly capability?: CapabilitySupport;
  readonly authState?: AuthState;
  readonly network?: NetworkState;
  /** Private account content (library, liked songs, playlists) is hidden once auth is lost. */
  readonly accountScoped?: boolean;
}

const SIGNED_OUT: ReadonlySet<AuthState> = new Set(["expired", "signedOut", "blocked"]);

/** Errors after which content already on screen stays visible as cached. */
const KEEP_CONTENT_ON: ReadonlySet<QueryError["code"]> = new Set(["NETWORK_OFFLINE", "TIMEOUT", "RATE_LIMITED", "BUSY"]);

/** True when private account data must not be fetched or shown for this auth state. */
export function isSignedOut(authState: AuthState | undefined): boolean {
  return authState !== undefined && SIGNED_OUT.has(authState);
}

/**
 * Single decision point for which state a catalog page shows. While signed out, private pages
 * and pages the core reports unsupported (it gates catalog reads on sign-in) ask for sign-in.
 */
export function resolvePageView(input: PageViewInput): PageView {
  const { error } = input;
  if (isSignedOut(input.authState) && (input.accountScoped || input.capability === "unsupported")) {
    return { kind: "authExpired", cached: false, error };
  }
  if (input.capability === "unsupported") return { kind: "unsupported", cached: false, error };
  if (error && input.hasData && KEEP_CONTENT_ON.has(error.code)) {
    return { kind: input.isEmpty ? "empty" : "ready", cached: true, error };
  }
  if (error) {
    switch (error.code) {
      case "AUTH_REQUIRED":
      case "AUTH_EXPIRED":
      case "ACCOUNT_MISMATCH":
        return { kind: "authExpired", cached: false, error };
      case "UNSUPPORTED_CAPABILITY":
        return { kind: "unsupported", cached: false, error };
      case "NETWORK_OFFLINE":
        return { kind: "offline", cached: false, error };
      default:
        return { kind: "error", cached: false, error };
    }
  }
  if (!input.hasData) {
    if (input.status !== "done") return { kind: "loading", cached: false, error: null };
    return { kind: input.network === "offline" ? "offline" : "empty", cached: false, error: null };
  }
  const cached = input.stale || input.network === "offline";
  if (input.isEmpty) return { kind: "empty", cached, error: null };
  return { kind: "ready", cached, error: null };
}
