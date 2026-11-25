import type { AccountEpoch, QueryErrorCode, RecentSearchesRequest, RecentSearchesSnapshot } from "@ytmcp/protocol";
import { CoreRequestError } from "./ipc/client.js";

/** Longest query the core keeps, in Unicode code points; a longer one is not recorded at all. */
export const MAX_RECENT_QUERY_CHARS = 200;

/** Where the list lives: the core's `ui.recentSearches`, asked for one account epoch. */
export interface RecentSearchesSource {
  recentSearches(req: RecentSearchesRequest, accountEpoch: AccountEpoch): Promise<RecentSearchesSnapshot>;
}

/** The query as it is sent to be recorded (NFC, trimmed), or null when it is empty or too long. */
export function recentSearchQuery(query: string): string | null {
  const text = query.normalize("NFC").trim();
  if (text === "" || [...text].length > MAX_RECENT_QUERY_CHARS) return null;
  return text;
}

/**
 * The Search page's recent searches: a cache of the core's reply for the signed-in account. The
 * core keeps the list per account across restarts; every reply is the whole list, so it simply
 * replaces what is shown. `sync` follows the authenticated account epoch (null when there is
 * none) and asks for the list whenever a new one appears; a reply for any other epoch, or one
 * older than a reply already shown, is dropped. Failures keep the last list, except that a
 * refused sign-in empties it until the account can be asked again.
 */
export class RecentSearches {
  #items = $state.raw<readonly string[]>([]);
  readonly #source: RecentSearchesSource | null;
  #epoch: AccountEpoch | null = null;
  /** The epoch whose list has been asked for; null makes the next `sync` (a new epoch, or the same one again) ask again. */
  #listed: AccountEpoch | null = null;
  #sent = 0;
  #applied = 0;

  constructor(source: RecentSearchesSource | null) {
    this.#source = source;
  }

  /** Newest first, at most the core's limit. */
  get items(): readonly string[] {
    return this.#items;
  }

  /** Follows the authenticated account; another account (or none) empties the list at once. */
  sync(accountEpoch: AccountEpoch | null): void {
    if (accountEpoch !== this.#epoch) {
      this.#epoch = accountEpoch;
      this.#listed = null;
      this.#applied = this.#sent;
      this.#items = [];
    }
    if (accountEpoch !== null && this.#listed !== accountEpoch) this.#list();
  }

  /**
   * The core dropped this account's state (sign-out, account change, expiry): the list is
   * emptied here, with no clear sent, and asked for again once the deltas of this turn have
   * settled. Replies sent before are dropped; a sign-out seen meanwhile leaves it empty.
   */
  invalidate(): void {
    this.#applied = this.#sent;
    this.#listed = null;
    this.#items = [];
    queueMicrotask(() => {
      if (this.#epoch !== null && this.#listed === null) this.#list();
    });
  }

  /** Records a query the listener actually ran; empty or overlong ones are not sent. */
  record(query: string): void {
    const text = recentSearchQuery(query);
    if (text !== null) this.#send({ action: "record", query: text });
  }

  /** Asks the core to forget this account's list; the reply (empty) replaces it. */
  clear(): void {
    this.#send({ action: "clear" });
  }

  #list(): void {
    this.#listed = this.#epoch;
    this.#send({ action: "list" });
  }

  #send(req: RecentSearchesRequest): void {
    const epoch = this.#epoch;
    if (epoch === null || !this.#source) return;
    const seq = ++this.#sent;
    const current = () => epoch === this.#epoch && seq > this.#applied;
    let reply: Promise<RecentSearchesSnapshot>;
    try {
      reply = this.#source.recentSearches(req, epoch);
    } catch (err) {
      reply = Promise.reject(err);
    }
    reply.then(
      (snapshot) => {
        if (!current()) return;
        this.#applied = seq;
        this.#items = snapshot.items.map((item) => item.query);
      },
      (err: unknown) => {
        if (!current()) return;
        const code: QueryErrorCode | null = err instanceof CoreRequestError ? err.error.code : null;
        if (code === "AUTH_REQUIRED") {
          // No session behind this epoch yet: nothing to show; asked again when an epoch appears.
          this.#applied = seq;
          this.#items = [];
          this.#listed = null;
        } else if (code === "ACCOUNT_MISMATCH") {
          // The core is on another account than this epoch; its own epoch arrives as a delta
          // and `sync` asks for that list. Nothing is replayed for the new account.
          this.#listed = null;
        }
        // Anything else (stale context, storage, busy, transport): the last list stays.
      }
    );
  }
}
