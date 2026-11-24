/** Most queries kept, newest first. */
export const RECENT_SEARCH_LIMIT = 8;
/** Longest query kept; longer input is cut, never stored whole. */
const MAX_QUERY_LENGTH = 200;
const STORAGE_KEY = "ytmcp.recentSearches.v1";

interface StoredRecentSearches {
  readonly account: string;
  readonly queries: readonly string[];
}

/** Collapses whitespace and trims; returns "" for a query that is not worth keeping. */
export function normalizeQuery(query: string): string {
  return query.normalize("NFC").replace(/\s+/g, " ").trim().slice(0, MAX_QUERY_LENGTH);
}

/**
 * The account a recent-searches list belongs to: the catalog account key while signed in
 * (`acct:<scope>:<epoch>`), null otherwise. The epoch changes on sign-out and on account
 * change, so a list from before either never matches again.
 */
export function recentSearchAccount(accountKey: string | null | undefined): string | null {
  return accountKey?.startsWith("acct:") ? accountKey : null;
}

function defaultStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function readStored(storage: Storage | null): StoredRecentSearches | null {
  if (!storage) return null;
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return null;
    const { account, queries } = value as Record<string, unknown>;
    if (typeof account !== "string" || !Array.isArray(queries)) return null;
    const kept = queries
      .filter((q): q is string => typeof q === "string")
      .map(normalizeQuery)
      .filter((q) => q !== "")
      .slice(0, RECENT_SEARCH_LIMIT);
    return { account, queries: kept };
  } catch {
    return null;
  }
}

/**
 * Queries the user actually ran, kept in the renderer for one signed-in account. A list is
 * only ever shown to the account that made it; seeing another account (or nobody signed in
 * while recording) replaces it with an empty one. Storage failures fall back to memory.
 */
export class RecentSearches {
  #account = $state<string | null>(null);
  #queries = $state.raw<readonly string[]>([]);
  readonly #storage: Storage | null;

  constructor(storage: Storage | null = defaultStorage()) {
    this.#storage = storage;
    const stored = readStored(storage);
    if (stored) {
      this.#account = stored.account;
      this.#queries = stored.queries;
    }
  }

  /** The list for this catalog account key; empty for any other account or while signed out. */
  itemsFor(accountKey: string | null | undefined): readonly string[] {
    const account = recentSearchAccount(accountKey);
    return account !== null && account === this.#account ? this.#queries : [];
  }

  /**
   * Forgets a list that belongs to someone else: called with the current account key, it
   * empties the store once a different account (or a sign-out) is seen. Unknown and
   * signing-in states keep it, so a slow start-up does not wipe it.
   */
  sync(accountKey: string | null | undefined): void {
    if (this.#account === null) return;
    if (accountKey === "none:signedOut" || (recentSearchAccount(accountKey) ?? this.#account) !== this.#account) {
      this.#replace(null, []);
    }
  }

  /** Puts a query the user ran at the front, dropping a case-insensitive duplicate. */
  record(accountKey: string | null | undefined, query: string): void {
    const account = recentSearchAccount(accountKey);
    const text = normalizeQuery(query);
    if (account === null || text === "") return;
    const previous = account === this.#account ? this.#queries : [];
    const folded = text.toLowerCase();
    this.#replace(account, [text, ...previous.filter((q) => q.toLowerCase() !== folded)].slice(0, RECENT_SEARCH_LIMIT));
  }

  /** Empties the list of the current account. */
  clear(): void {
    if (this.#account !== null) this.#replace(this.#account, []);
  }

  #replace(account: string | null, queries: readonly string[]): void {
    this.#account = account;
    this.#queries = queries;
    if (!this.#storage) return;
    try {
      if (account === null) this.#storage.removeItem(STORAGE_KEY);
      else this.#storage.setItem(STORAGE_KEY, JSON.stringify({ account, queries } satisfies StoredRecentSearches));
    } catch {
      // Storage is a convenience: the list keeps working in memory.
    }
  }
}

/** The app's one list, shared by every mount of the search page. */
export const recentSearches = new RecentSearches();
