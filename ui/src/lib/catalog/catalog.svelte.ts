import type {
  AlbumDetail,
  ArtistDetail,
  AuthStatusInfo,
  CatalogQueryRequest,
  CatalogResponsePayload,
  HomeFeed,
  PlaylistDetail,
  PlaylistSummary,
  QueryError,
  SearchResult,
  Track,
} from "@ytm-glass/protocol";
import type { PageStatus } from "./page-state.js";

export const SEARCH_PAGE_SIZE = 50;
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

/**
 * One catalog page: a detail header plus a cursor-paginated item list. `key` identifies what
 * the page shows (query, playlist id, ...). Large item arrays are stored raw (not deeply reactive).
 */
export class CatalogPage<D, I> {
  key = $state<string | null>(null);
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

  get hasData(): boolean {
    return this.detail !== null || this.items.length > 0;
  }

  reset(): void {
    this.seq += 1;
    this.key = null;
    this.status = "idle";
    this.error = null;
    this.detail = null;
    this.items = [];
    this.nextCursor = null;
    this.stale = false;
    this.loadingMore = false;
    this.moreError = null;
  }

  invalidate(): void {
    this.seq += 1;
    this.status = "idle";
    this.error = null;
    this.nextCursor = null;
    this.stale = true;
    this.loadingMore = false;
    this.moreError = null;
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
  readonly search = new CatalogPage<SearchResult, Track>();
  readonly library = new CatalogPage<null, PlaylistSummary>();
  readonly playlist = new CatalogPage<PlaylistDetail, Track>();
  readonly album = new CatalogPage<AlbumDetail, Track>();
  readonly artist = new CatalogPage<ArtistDetail, Track>();
  readonly liked = new CatalogPage<null, Track>();
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

  /** Revoke the old cursor and in-flight response while preserving the route's query key. */
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
          return id === null || id === this.playlist.key ? ["playlist"] : [];
        case "likedSongs":
          return ["liked"];
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
    for (const route of affected) pages[route].invalidate();
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

  private async load<D, I>(
    page: CatalogPage<D, I>,
    key: string,
    req: CatalogQueryRequest,
    parse: (res: CatalogResponsePayload) => Parsed<D, I> | null,
    force: boolean
  ): Promise<void> {
    // Reuse a loaded (or loading) page; a page that ended in error is fetched again.
    if (!force && page.key === key && page.status !== "idle" && !page.error) return;
    const seq = ++page.seq;
    const accountKey = this.accountKey;
    if (page.key !== key) {
      page.detail = null;
      page.items = [];
      page.nextCursor = null;
      page.stale = false;
    }
    page.key = key;
    page.status = "loading";
    page.error = null;
    page.loadingMore = false;
    page.moreError = null;

    const res = await this.client.queryCatalog(req);
    if (seq !== page.seq || accountKey !== this.accountKey) return;

    page.status = "done";
    if (res.resultType === "error") {
      page.error = res;
      return;
    }
    const parsed = parse(res);
    if (!parsed) {
      page.error = invalidPayload(req.kind);
      return;
    }
    page.detail = parsed.detail;
    page.items = parsed.items;
    page.nextCursor = parsed.nextCursor;
    page.stale = parsed.stale;
  }

  private async loadMore<D, I>(
    page: CatalogPage<D, I>,
    req: (cursor: string) => CatalogQueryRequest,
    parse: (res: CatalogResponsePayload) => Parsed<D, I> | null
  ): Promise<void> {
    const cursor = page.nextCursor;
    if (!cursor || page.loadingMore || page.status !== "done") return;
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

  /** Starts a new search; an empty query clears results and drops any in-flight search. */
  runSearch(query: string): Promise<void> {
    const trimmed = query.trim();
    if (!trimmed) {
      this.search.reset();
      return Promise.resolve();
    }
    return this.load(
      this.search,
      trimmed,
      { kind: "searchSongs", query: trimmed, limit: SEARCH_PAGE_SIZE },
      parseSearch,
      true
    );
  }

  loadMoreSearch(): Promise<void> {
    const query = this.search.key ?? "";
    return this.loadMore(
      this.search,
      (cursor) => ({ kind: "searchSongs", query, cursor, limit: SEARCH_PAGE_SIZE }),
      parseSearch
    );
  }

  loadLibrary(force = false): Promise<void> {
    return this.load(this.library, "library", { kind: "libraryPlaylists" }, parseLibrary, force);
  }

  loadMoreLibrary(): Promise<void> {
    return this.loadMore(this.library, (cursor) => ({ kind: "libraryPlaylists", cursor }), parseLibrary);
  }

  openPlaylist(playlistId: string, force = false): Promise<void> {
    return this.load(
      this.playlist,
      playlistId,
      { kind: "playlistTracks", playlistId, limit: TRACK_PAGE_SIZE },
      parsePlaylist,
      force
    );
  }

  loadMorePlaylist(): Promise<void> {
    const playlistId = this.playlist.key ?? "";
    return this.loadMore(
      this.playlist,
      (cursor) => ({ kind: "playlistTracks", playlistId, cursor, limit: TRACK_PAGE_SIZE }),
      parsePlaylist
    );
  }

  openAlbum(albumId: string, force = false): Promise<void> {
    return this.load(this.album, albumId, { kind: "albumDetail", albumId }, parseAlbum, force);
  }

  openArtist(artistId: string, force = false): Promise<void> {
    return this.load(this.artist, artistId, { kind: "artistDetail", artistId }, parseArtist, force);
  }

  loadLiked(force = false): Promise<void> {
    return this.load(this.liked, "liked", { kind: "likedSongs", limit: TRACK_PAGE_SIZE }, parseLiked, force);
  }

  loadMoreLiked(): Promise<void> {
    return this.loadMore(
      this.liked,
      (cursor) => ({ kind: "likedSongs", cursor, limit: TRACK_PAGE_SIZE }),
      parseLiked
    );
  }

  loadRecent(force = false): Promise<void> {
    return this.load(this.recent, "recent", { kind: "recentHistory" }, parseRecent, force);
  }
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

function parseLiked(res: CatalogResponsePayload): Parsed<null, Track> | null {
  if (res.resultType !== "likedSongs") return null;
  return { detail: null, items: res.items, nextCursor: res.nextCursor ?? null, stale: res.freshness === "stale" };
}

function parseRecent(res: CatalogResponsePayload): Parsed<null, Track> | null {
  if (res.resultType !== "recentHistory") return null;
  return { detail: null, items: res.items, nextCursor: null, stale: res.freshness === "stale" };
}
