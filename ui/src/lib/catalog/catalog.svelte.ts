import type {
  AlbumDetail,
  ArtistDetail,
  AuthStatusInfo,
  CatalogQueryRequest,
  CollectionPlayback,
  CatalogResponsePayload,
  HomeFeed,
  PlaylistDetail,
  PlaylistSummary,
  QueryError,
  SearchResult,
  Track,
} from "@ytmcp/protocol";
import type { PageStatus } from "./page-state.js";

export const SEARCH_PAGE_SIZE = 50;

/**
 * The Liked songs page's own playback context, from its first reply: the collection's playlist
 * and each loaded first-page row's native index, with the account it was read for. Kept with
 * the page (and dropped with it), so the whole collection is played only from what the core read.
 */
export interface LikedPlayback {
  readonly playback: CollectionPlayback;
  readonly accountScope: string;
}
export const TRACK_PAGE_SIZE = 100;

export type CatalogRoute = "home" | "search" | "library" | "playlist" | "album" | "artist" | "liked" | "recent";

/** Minimal client surface the store needs (the real `YtmUiClient` satisfies it). */
export interface CatalogClient {
  queryCatalog(req: CatalogQueryRequest): Promise<CatalogResponsePayload>;
}

interface Parsed<D, I> {
  readonly detail: D | null;
  readonly items: readonly I[];
  readonly nextCursor: string | null;
  readonly stale: boolean;
}

/** Pages of one kind kept besides the one in view, so Back and re-opening show them at once. */
export const KEPT_PAGES = 3;
/** An in-place refresh reads at most this many pages again; rows past them wait for paging. */
export const REFRESH_MAX_PAGES = 20;

/** What happens to the shown content when a page is asked for another key. */
export type KeyChange =
  /** Drop it; keep the last few keys' content to show again (marked stale) when they come back. */
  | "keep"
  /** Leave it on screen, marked stale, until the new key's reply replaces it (search). */
  | "replace";

interface Kept<D, I> {
  readonly detail: D | null;
  readonly items: readonly I[];
  readonly nextCursor: string | null;
}

/**
 * One catalog page: a detail header plus a cursor-paginated item list. `key` identifies what
 * the page asks for (query, playlist id, ...); `dataKey` is the key the shown content belongs
 * to, which differs from `key` while a new search replaces the previous results. `stale` marks
 * content that is cached or being read again, never fresh. Large item arrays are stored raw.
 */
export class CatalogPage<D, I> {
  key = $state<string | null>(null);
  dataKey = $state<string | null>(null);
  status = $state<PageStatus>("idle");
  error = $state<QueryError | null>(null);
  detail = $state.raw<D | null>(null);
  items = $state.raw<readonly I[]>([]);
  nextCursor = $state<string | null>(null);
  stale = $state(false);
  loadingMore = $state(false);
  moreError = $state<QueryError | null>(null);
  /** Request generation; any response from an older generation is dropped. */
  seq = 0;
  /** Content of recently shown other keys, oldest first. */
  #kept = new Map<string, Kept<D, I>>();

  constructor(private readonly keyChange: KeyChange = "keep") {}

  get hasData(): boolean {
    return this.detail !== null || this.items.length > 0;
  }

  reset(): void {
    this.seq += 1;
    this.key = null;
    this.status = "idle";
    this.error = null;
    this.clearShown();
    this.loadingMore = false;
    this.moreError = null;
    this.#kept.clear();
  }

  /**
   * Revokes the in-flight response and marks the content stale; the rows and cursor stay on
   * screen (paging waits, as the page is no longer "done") until a refresh replaces them.
   * Kept copies of other keys are dropped, or only `id`'s when the invalidation names one.
   */
  invalidate(id?: string | null): void {
    if (id != null && id !== this.key) {
      this.#kept.delete(id);
      return;
    }
    this.seq += 1;
    this.status = "idle";
    this.error = null;
    this.stale = true;
    this.loadingMore = false;
    this.moreError = null;
    if (id == null) this.#kept.clear();
  }

  /**
   * The listener left the page: an in-place re-read still running (it can take many pages) is
   * revoked, and the shown content stays marked stale, so it is read again on the next visit.
   */
  stopRefresh(): void {
    if (this.status !== "loading" || this.dataKey !== this.key || !this.hasData) return;
    this.seq += 1;
    this.status = "idle";
    this.stale = true;
  }

  /** Points the page at another key: parks the shown content and brings back the new key's, if kept. */
  switchKey(key: string): void {
    const shown = this.dataKey;
    if (this.keyChange === "replace") {
      this.stale = this.hasData;
    } else {
      if (shown !== null && shown !== key && this.hasData) {
        this.#kept.delete(shown);
        this.#kept.set(shown, { detail: this.detail, items: this.items, nextCursor: this.nextCursor });
        while (this.#kept.size > KEPT_PAGES) this.#kept.delete(this.#kept.keys().next().value!);
      }
      const kept = this.#kept.get(key);
      this.#kept.delete(key);
      if (kept) {
        this.detail = kept.detail;
        this.items = kept.items;
        this.nextCursor = kept.nextCursor;
        this.dataKey = key;
        this.stale = true;
      } else {
        this.clearShown();
      }
    }
    this.key = key;
  }

  clearShown(): void {
    this.dataKey = null;
    this.detail = null;
    this.items = [];
    this.nextCursor = null;
    this.stale = false;
  }
}

/** Identity of the account whose private data the UI may show; changes clear every page. */
export function catalogAccountKey(auth: AuthStatusInfo | null | undefined): string {
  if (auth?.state === "authenticated") {
    return `acct:${auth.accountScope ?? ""}:${auth.accountEpoch ?? ""}`;
  }
  return `none:${auth?.state ?? "unknown"}`;
}

function invalidPayload(expected: string): QueryError {
  return {
    code: "INVALID_PAYLOAD",
    retriable: false,
    retryAfterMs: null,
    safeMessage: `unexpected catalog response (expected ${expected})`,
    requestId: null,
  };
}

/**
 * Account-scoped catalog state for every browse page. Responses are applied only if they belong
 * to the latest request of that page and to the account that was active when it was sent.
 */
export class CatalogStore {
  readonly home = new CatalogPage<HomeFeed, never>();
  readonly search = new CatalogPage<SearchResult, Track>("replace");
  readonly library = new CatalogPage<null, PlaylistSummary>();
  readonly playlist = new CatalogPage<PlaylistDetail, Track>();
  readonly album = new CatalogPage<AlbumDetail, Track>();
  readonly artist = new CatalogPage<ArtistDetail, Track>();
  readonly liked = new CatalogPage<LikedPlayback, Track>();
  readonly recent = new CatalogPage<null, Track>();

  private accountKey: string | null = null;

  constructor(private readonly client: CatalogClient) {}

  private pages(): Record<CatalogRoute, CatalogPage<unknown, unknown>> {
    return {
      home: this.home,
      search: this.search,
      library: this.library,
      playlist: this.playlist,
      album: this.album,
      artist: this.artist,
      liked: this.liked,
      recent: this.recent,
    };
  }

  /** The listener left `route`'s page; see `CatalogPage.stopRefresh`. Other views are ignored. */
  leave(route: string): void {
    const page = (this.pages() as Record<string, CatalogPage<unknown, unknown> | undefined>)[route];
    page?.stopRefresh();
  }

  /** Clears all pages and invalidates in-flight requests when the account identity changes. */
  syncAccount(accountKey: string): boolean {
    if (accountKey === this.accountKey) return false;
    this.accountKey = accountKey;
    this.clear();
    return true;
  }

  /** Drops every page and in-flight response (e.g. after switching mock scenarios). */
  clear(): void {
    for (const page of Object.values(this.pages())) page.reset();
  }

  /**
   * Revokes in-flight responses of the pages `kind` affects and marks their content stale; the
   * returned routes need a refresh (in place: see `load`). History and membership changes drop
   * their pages outright, so a late read from before the change can never show again.
   */
  invalidate(kind: string, id: string | null): CatalogRoute[] {
    if (kind === "localHistory") {
      this.home.reset();
      this.recent.reset();
      return ["home", "recent"];
    }
    if (kind === "playlistMembership") {
      this.home.reset();
      this.library.reset();
      return ["home", "library"];
    }
    const affected = ((): CatalogRoute[] => {
      switch (kind) {
        case "catalog":
        case "resync":
          return ["home", "search", "library", "playlist", "album", "artist", "liked", "recent"];
        case "library":
        case "libraryPlaylists":
          return ["library", "home"];
        case "playlistTracks":
          // A confirmed add names its playlist; only one without a name (or the open one) reads
          // the open playlist again in place.
          if (id !== null && id !== this.playlist.key) {
            this.playlist.invalidate(id);
            return [];
          }
          return ["playlist"];
        case "likedSongs": {
          // A like or unlike made on the Liked page itself must not rebuild the list under the
          // listener (the row's heart already shows it): shown rows stay, marked stale, and are
          // read again in place when the page is next opened or paged. A page still loading
          // (or empty) has nothing to keep and is read again now.
          const shown = this.liked.status === "done" && this.liked.hasData;
          this.liked.invalidate();
          return shown ? [] : ["liked"];
        }
        case "rating": {
          const tracks = [
            ["home", this.home, this.home.detail?.recentTracks ?? []],
            ["search", this.search, this.search.items],
            ["playlist", this.playlist, this.playlist.items],
            ["album", this.album, this.album.items],
            ["artist", this.artist, this.artist.items],
            ["recent", this.recent, this.recent.items],
          ] as const;
          return tracks
            .filter(([, page, items]) => page.status === "loading" || page.loadingMore || items.some((track) => track.videoId === id))
            .map(([route]) => route);
        }
        default:
          return [];
      }
    })();
    const pages = this.pages();
    for (const route of affected) pages[route].invalidate(kind === "playlistTracks" ? id : null);
    return affected;
  }

  freshRating(videoId: string): boolean | null {
    const trackPages = [this.search, this.playlist, this.album, this.artist, this.recent];
    for (const page of trackPages) {
      if (!page.stale) {
        const rating = page.items.find((item) => item.videoId === videoId)?.isLiked;
        if (rating != null) return rating;
      }
    }
    if (!this.home.stale) {
      const rating = this.home.detail?.recentTracks?.find((track) => track.videoId === videoId)?.isLiked;
      if (rating != null) return rating;
    }
    if (!this.liked.stale && this.liked.items.some((track) => track.videoId === videoId)) return true;
    return null;
  }

  /**
   * Fetches `key` into `page`. Content already shown for the same key stays on screen while it
   * is read again, and the refresh reads as many pages as were loaded (up to
   * REFRESH_MAX_PAGES) before replacing it in one step, so the list keeps its length and the
   * listener keeps their place. `more` builds the request for a following page.
   */
  private async load<D, I>(
    page: CatalogPage<D, I>,
    key: string,
    req: CatalogQueryRequest,
    parse: (res: CatalogResponsePayload) => Parsed<D, I> | null,
    force: boolean,
    more?: (cursor: string) => CatalogQueryRequest
  ): Promise<void> {
    // Reuse a loaded (or loading) page; a page that ended in error is fetched again.
    if (!force && page.key === key && page.status !== "idle" && !page.error) return;
    const seq = ++page.seq;
    const accountKey = this.accountKey;
    if (page.key !== key) page.switchKey(key);
    page.status = "loading";
    page.error = null;
    page.loadingMore = false;
    page.moreError = null;
    const shownRows = page.dataKey === key ? page.items.length : 0;

    const fail = (error: QueryError) => {
      page.status = "done";
      // Another key's content (the previous search) must not pass for this key's cached copy.
      if (page.dataKey !== key) page.clearShown();
      page.error = error;
    };
    const current = () => seq === page.seq && accountKey === this.accountKey;
    const accept = (res: CatalogResponsePayload, request: CatalogQueryRequest): Parsed<D, I> | QueryError =>
      res.resultType === "error" ? res : (parse(res) ?? invalidPayload(request.kind));

    const res = await this.client.queryCatalog(req);
    if (!current()) return;
    const first = accept(res, req);
    if ("code" in first) return fail(first);
    let items = first.items;
    let nextCursor = first.nextCursor;
    let stale = first.stale;
    for (let pages = 1; more && nextCursor && items.length < shownRows && pages < REFRESH_MAX_PAGES; pages++) {
      const request = more(nextCursor);
      const res = await this.client.queryCatalog(request);
      if (!current()) return;
      const next = accept(res, request);
      if ("code" in next) return fail(next);
      items = [...items, ...next.items];
      nextCursor = next.nextCursor;
      stale = stale || next.stale;
    }
    page.status = "done";
    page.dataKey = key;
    page.detail = first.detail;
    page.items = items;
    page.nextCursor = nextCursor;
    page.stale = stale;
  }

  /**
   * Appends the next page. A page whose content was invalidated while shown is read again in
   * place first (its old cursor is revoked); paging continues from the refreshed cursor, unless
   * the page was pointed at another key (or account) meanwhile: `req` belongs to the old one.
   */
  private async loadMore<D, I>(
    page: CatalogPage<D, I>,
    req: (cursor: string) => CatalogQueryRequest,
    parse: (res: CatalogResponsePayload) => Parsed<D, I> | null,
    refresh?: () => Promise<void>
  ): Promise<void> {
    if (page.status === "idle" && page.hasData && refresh) {
      const key = page.key;
      const accountKey = this.accountKey;
      await refresh();
      if (page.key !== key || this.accountKey !== accountKey) return;
      // The re-read keeps the row count, so the list does not ask again: go on from its cursor
      // (only once it is done; never a second refresh).
      return this.loadMore(page, req, parse);
    }
    const cursor = page.nextCursor;
    // After a failed refresh the cursor is the revoked one; Retry reads the page again first.
    if (!cursor || page.loadingMore || page.status !== "done" || page.error) return;
    const seq = page.seq;
    const accountKey = this.accountKey;
    page.loadingMore = true;
    page.moreError = null;

    const res = await this.client.queryCatalog(req(cursor));
    if (seq !== page.seq || accountKey !== this.accountKey) return;

    page.loadingMore = false;
    if (res.resultType === "error") {
      page.moreError = res;
      return;
    }
    const parsed = parse(res);
    if (!parsed) {
      page.moreError = invalidPayload(req(cursor).kind);
      return;
    }
    page.items = [...page.items, ...parsed.items];
    page.nextCursor = parsed.nextCursor;
    page.stale = page.stale || parsed.stale;
  }

  loadHome(force = false): Promise<void> {
    return this.load(this.home, "home", { kind: "home" }, parseHome, force);
  }

  /**
   * Starts a new search; an empty query clears results and drops any in-flight search. The
   * previous results stay on screen, marked stale, until the new reply arrives or fails.
   */
  runSearch(query: string): Promise<void> {
    const trimmed = query.trim();
    if (!trimmed) {
      this.search.reset();
      return Promise.resolve();
    }
    return this.load(this.search, trimmed, searchRequest(trimmed), parseSearch, true, (cursor) =>
      searchRequest(trimmed, cursor)
    );
  }

  loadMoreSearch(): Promise<void> {
    const query = this.search.key ?? "";
    return this.loadMore(this.search, (cursor) => searchRequest(query, cursor), parseSearch, () =>
      this.runSearch(query)
    );
  }

  loadLibrary(force = false): Promise<void> {
    return this.load(this.library, "library", { kind: "libraryPlaylists" }, parseLibrary, force, libraryRequest);
  }

  loadMoreLibrary(): Promise<void> {
    return this.loadMore(this.library, libraryRequest, parseLibrary, () => this.loadLibrary());
  }

  openPlaylist(playlistId: string, force = false): Promise<void> {
    return this.load(this.playlist, playlistId, playlistRequest(playlistId), parsePlaylist, force, (cursor) =>
      playlistRequest(playlistId, cursor)
    );
  }

  loadMorePlaylist(): Promise<void> {
    const playlistId = this.playlist.key ?? "";
    return this.loadMore(this.playlist, (cursor) => playlistRequest(playlistId, cursor), parsePlaylist, () =>
      this.openPlaylist(playlistId)
    );
  }

  openAlbum(albumId: string, force = false): Promise<void> {
    return this.load(this.album, albumId, { kind: "albumDetail", albumId }, parseAlbum, force);
  }

  openArtist(artistId: string, force = false): Promise<void> {
    return this.load(this.artist, artistId, { kind: "artistDetail", artistId }, parseArtist, force);
  }

  loadLiked(force = false): Promise<void> {
    return this.load(this.liked, "liked", likedRequest(), parseLiked, force, likedRequest);
  }

  loadMoreLiked(): Promise<void> {
    return this.loadMore(this.liked, likedRequest, parseLiked, () => this.loadLiked());
  }

  loadRecent(force = false): Promise<void> {
    return this.load(this.recent, "recent", { kind: "recentHistory" }, parseRecent, force);
  }
}

function searchRequest(query: string, cursor?: string): CatalogQueryRequest {
  return { kind: "searchSongs", query, ...(cursor ? { cursor } : {}), limit: SEARCH_PAGE_SIZE };
}

function libraryRequest(cursor?: string): CatalogQueryRequest {
  return cursor ? { kind: "libraryPlaylists", cursor } : { kind: "libraryPlaylists" };
}

function playlistRequest(playlistId: string, cursor?: string): CatalogQueryRequest {
  return { kind: "playlistTracks", playlistId, ...(cursor ? { cursor } : {}), limit: TRACK_PAGE_SIZE };
}

function likedRequest(cursor?: string): CatalogQueryRequest {
  return { kind: "likedSongs", ...(cursor ? { cursor } : {}), limit: TRACK_PAGE_SIZE };
}

function parseHome(res: CatalogResponsePayload): Parsed<HomeFeed, never> | null {
  if (res.resultType !== "home") return null;
  return { detail: res, items: [], nextCursor: null, stale: res.freshness === "stale" };
}

function parseSearch(res: CatalogResponsePayload): Parsed<SearchResult, Track> | null {
  if (res.resultType !== "searchSongs") return null;
  return {
    detail: res,
    items: res.songs.items,
    nextCursor: res.songs.nextCursor ?? null,
    stale: res.songs.freshness === "stale",
  };
}

function parseLibrary(res: CatalogResponsePayload): Parsed<null, PlaylistSummary> | null {
  if (res.resultType !== "libraryPlaylists") return null;
  return { detail: null, items: res.items, nextCursor: res.nextCursor ?? null, stale: res.freshness === "stale" };
}

function parsePlaylist(res: CatalogResponsePayload): Parsed<PlaylistDetail, Track> | null {
  if (res.resultType !== "playlistTracks") return null;
  return {
    detail: res,
    items: res.tracks.items,
    nextCursor: res.tracks.nextCursor ?? null,
    stale: res.tracks.freshness === "stale",
  };
}

/**
 * Playback context for track `index` of an album: the album's audio playlist and the track's
 * provider watch index. Null (play the single track) unless the indexes align 1:1 with the
 * shown tracks and the index is a valid non-negative integer.
 */
export function albumPlaybackContext(
  album: AlbumDetail | null | undefined,
  tracks: readonly Track[],
  index: number
): { playlistId: string; startIndex: number } | null {
  const playlistId = album?.audioPlaylistId;
  const indexes = album?.playbackIndexes;
  if (!playlistId || !indexes || indexes.length !== tracks.length || indexes.length !== album.tracks.length) return null;
  const startIndex = indexes[index];
  if (startIndex === undefined || !Number.isInteger(startIndex) || startIndex < 0) return null;
  return { playlistId, startIndex };
}

function parseAlbum(res: CatalogResponsePayload): Parsed<AlbumDetail, Track> | null {
  if (res.resultType !== "albumDetail") return null;
  return { detail: res, items: res.tracks, nextCursor: null, stale: false };
}

function parseArtist(res: CatalogResponsePayload): Parsed<ArtistDetail, Track> | null {
  if (res.resultType !== "artistDetail") return null;
  return { detail: res, items: res.topSongs ?? [], nextCursor: null, stale: false };
}

/**
 * A liked page; its playback context is kept only when it lines up one to one with the rows and
 * its first index is usable. Later pages' contexts are not needed: Play starts at the first row.
 */
function parseLiked(res: CatalogResponsePayload): Parsed<LikedPlayback, Track> | null {
  if (res.resultType !== "likedSongs") return null;
  const playback = res.playback;
  const start = playback?.entries[0]?.startIndex;
  const usable =
    playback &&
    playback.playlistId &&
    res.items.length > 0 &&
    playback.entries.length === res.items.length &&
    start !== undefined &&
    Number.isInteger(start) &&
    start >= 0;
  return {
    detail: usable ? { playback, accountScope: res.accountScope } : null,
    items: res.items,
    nextCursor: res.nextCursor ?? null,
    stale: res.freshness === "stale",
  };
}

function parseRecent(res: CatalogResponsePayload): Parsed<null, Track> | null {
  if (res.resultType !== "recentHistory") return null;
  return { detail: null, items: res.items, nextCursor: null, stale: res.freshness === "stale" };
}
