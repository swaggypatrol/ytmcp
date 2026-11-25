import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  CALIBRATED_LRC_FIXTURE,
  FakeCoreClient,
  FakeCoreRequestError,
  MAX_FRAME_BYTES,
  createSyntheticTracks,
  isKindAllowedForRole,
  parseAndValidateEnvelope,
  serializeEnvelope,
} from "../src/index.js";

const fixturesDir = resolve(import.meta.dirname, "../../../fixtures/contracts");

describe("protocol validator", () => {
  it("validates shared Rust/TS valid contract fixtures", () => {
    const raw = readFileSync(resolve(fixturesDir, "valid_envelopes.ndjson"), "utf8");
    const lines = raw.trim().split("\n");
    expect(lines.length).toBeGreaterThan(0);

    for (const line of lines) {
      const res = parseAndValidateEnvelope(line);
      expect(res.ok).toBe(true);
      if (res.ok) {
        const reserialized = serializeEnvelope(res.value);
        expect(reserialized.endsWith("\n")).toBe(true);
      }
    }
  });

  it("rejects shared invalid contract fixtures", () => {
    const raw = readFileSync(resolve(fixturesDir, "invalid_envelopes.ndjson"), "utf8");
    const lines = raw.trim().split("\n");
    for (const line of lines) {
      const res = parseAndValidateEnvelope(line);
      expect(res.ok).toBe(false);
    }
  });

  it("rejects oversized frames and unsupported protocolVersion", () => {
    const big = Buffer.alloc(MAX_FRAME_BYTES + 1, 0x20);
    const bigRes = parseAndValidateEnvelope(big);
    expect(bigRes.ok).toBe(false);

    const verRes = parseAndValidateEnvelope(
      JSON.stringify({ protocolVersion: 99, kind: "ui.ping", seq: 1, payload: {} })
    );
    expect(verRes.ok).toBe(false);
    if (!verRes.ok) {
      expect(verRes.code).toBe("BRIDGE_INCOMPATIBLE");
    }
  });

  it("enforces role boundaries", () => {
    expect(isKindAllowedForRole("ui.playerCommand", "mainUi")).toBe(true);
    expect(isKindAllowedForRole("host.sessionMaterial", "mainUi")).toBe(false);
    expect(isKindAllowedForRole("bridge.ready", "mainUi")).toBe(false);
    expect(isKindAllowedForRole("bridge.playbackObserved", "playerBridge")).toBe(true);
    expect(isKindAllowedForRole("ui.settingsUpdate", "playerBridge")).toBe(false);
    expect(isKindAllowedForRole("core.settingsUpdated", "core")).toBe(true);
    expect(isKindAllowedForRole("core.settingsUpdated", "mainUi")).toBe(false);
    expect(isKindAllowedForRole("ui.recentSearches", "mainUi")).toBe(true);
    expect(isKindAllowedForRole("core.recentSearches", "core")).toBe(true);
    expect(isKindAllowedForRole("ui.recentSearches", "playerBridge")).toBe(false);
  });

  it("validates partial settings patches and rejects out-of-range UI scale", () => {
    const valid = parseAndValidateEnvelope(
      JSON.stringify({
        protocolVersion: 1,
        kind: "ui.settingsUpdate",
        seq: 1,
        payload: { uiLanguage: "zh-CN" },
      })
    );
    expect(valid.ok).toBe(true);

    const invalid = parseAndValidateEnvelope(
      JSON.stringify({
        protocolVersion: 1,
        kind: "ui.settingsUpdate",
        seq: 2,
        payload: { uiScalePercent: 50 },
      })
    );
    expect(invalid.ok).toBe(false);
  });
});

describe("FakeCoreClient", () => {
  it("deduplicates commands by commandId and simulates indeterminate next without retry", () => {
    const client = new FakeCoreClient("normal");
    const seenRevisions: number[] = [];
    const { snapshot, unsubscribe } = client.subscribeSnapshot((d) => {
      seenRevisions.push(d.revision);
    });
    expect(snapshot.mockMode).toBe(true);
    expect(snapshot.player.playback).toBe("paused");

    const r1 = client.sendPlayerCommand({ commandId: "c-play", op: "play", args: null });
    expect(r1.status).toBe("observed");
    expect(r1.deduplicated).toBe(false);

    const r2 = client.sendPlayerCommand({ commandId: "c-play", op: "play", args: null });
    expect(r2.deduplicated).toBe(true);
    expect(seenRevisions.length).toBeGreaterThanOrEqual(2);
    unsubscribe();

    const indClient = new FakeCoreClient("indeterminateNext");
    const nextRes = indClient.sendPlayerCommand({ commandId: "c-next", op: "next", args: null });
    expect(nextRes.status).toBe("indeterminate");
  });

  it("serves 10,000 synthetic tracks with opaque cursor pagination and calibrated LRC", () => {
    const tracks = createSyntheticTracks(10_000);
    expect(tracks).toHaveLength(10_000);

    const client = new FakeCoreClient("normal");
    const page1 = client.queryCatalog({ kind: "playlistTracks", playlistId: "mock-pl-favorites", limit: 100 });
    expect(page1.resultType).toBe("playlistTracks");
    if (page1.resultType === "playlistTracks") {
      expect(page1.tracks.items).toHaveLength(100);
      expect(page1.tracks.nextCursor).toBe("100");
    }

    const lrc = client.queryLyrics({
      track: { videoId: "mock-vid-00001", title: "Test", artist: "Artist" },
      resetUserOffset: false,
    });
    expect(lrc.kind).toBe("synced");
    expect(lrc.syncedLines?.length).toBe(CALIBRATED_LRC_FIXTURE.syncedLines?.length);
  });

  it("serves linked album and artist views for a synthetic playlist track", () => {
    const client = new FakeCoreClient("normal");
    const playlist = client.queryCatalog({ kind: "playlistTracks", playlistId: "mock-pl-favorites", limit: 1 });
    if (playlist.resultType !== "playlistTracks") throw new Error("missing mock playlist");
    const track = playlist.tracks.items[0]!;
    const album = client.queryCatalog({ kind: "albumDetail", albumId: track.album?.id });
    if (album.resultType !== "albumDetail") throw new Error("missing mock album");
    expect(album.tracks[0]?.videoId).toBe(track.videoId);

    const artist = client.queryCatalog({ kind: "artistDetail", artistId: album.artists?.[0]?.id });
    if (artist.resultType !== "artistDetail") throw new Error("missing mock artist");
    expect(artist.topSongs?.[0]?.videoId).toBe(track.videoId);
    expect(artist.albums?.[0]?.id).toBe(album.albumId);
  });

  it("merges partial settings updates onto current fake settings", () => {
    const client = new FakeCoreClient("normal");
    client.updateSettings({ reduceTransparency: true });
    const updated = client.updateSettings({ uiLanguage: "zh-CN" });
    expect(updated.reduceTransparency).toBe(true);
    expect(updated.uiLanguage).toBe("zh-CN");
  });

  it("keeps recent searches for the signed-in mock account with the core's rules", () => {
    const client = new FakeCoreClient("normal");
    const epoch = "mock-account-epoch-1";
    const refused = (run: () => unknown) => {
      try {
        run();
      } catch (err) {
        if (err instanceof FakeCoreRequestError) return err.error.code;
        throw err;
      }
      return null;
    };
    const record = (query: string) => client.recentSearches({ action: "record", query }, epoch);
    const queries = () => client.recentSearches({ action: "list" }, epoch).items.map((item) => item.query);

    expect(queries()).toEqual([]);
    for (let i = 0; i < 10; i++) record(`query ${i}`);
    expect(queries()).toHaveLength(8);
    expect(queries()[0]).toBe("query 9");
    // Case and spacing do not make a second entry; the new spelling moves to the front.
    record("  QUERY   5 ");
    expect(queries()).toHaveLength(8);
    expect(queries()[0]).toBe("QUERY   5");
    expect(queries().filter((q) => q.toLowerCase().includes("5"))).toHaveLength(1);

    expect(refused(() => record("   "))).toBe("INVALID_PAYLOAD");
    expect(refused(() => record("x".repeat(201)))).toBe("INVALID_PAYLOAD");
    expect(refused(() => client.recentSearches({ action: "list" }, null))).toBe("INVALID_PAYLOAD");
    expect(refused(() => client.recentSearches({ action: "list" }, "another-epoch"))).toBe("ACCOUNT_MISMATCH");

    expect(client.recentSearches({ action: "clear" }, epoch).items).toEqual([]);
    expect(queries()).toEqual([]);

    const expired = new FakeCoreClient("authExpired");
    expect(refused(() => expired.recentSearches({ action: "list" }, epoch))).toBe("AUTH_REQUIRED");
  });
});
