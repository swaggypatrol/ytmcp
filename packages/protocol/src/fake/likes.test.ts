import { describe, expect, it } from "vitest";
import type { MutationRequest, QueueSummary, StateDelta } from "../generated/types.js";
import { FakeCoreClient } from "./index.js";

const FIRST = "mock-vid-00001";
const SECOND = "mock-vid-00002";
const OFFICIAL = "mock-vid-00021";
const ACCOUNT = "mock-account-epoch-1";

function like(videoId: string, liked: boolean): MutationRequest {
  return { kind: "setLike", videoId, liked, confirmedByUser: true };
}

function page(fake: FakeCoreClient, cursor?: string | null) {
  const result = fake.queryCatalog({ kind: "likedSongs", limit: 2, cursor });
  if (result.resultType !== "likedSongs") throw new Error("Expected mock liked songs");
  return result;
}

function queueLikes(summary: QueueSummary, videoId: string) {
  return summary.previewEntries?.filter((entry) => entry.videoId === videoId).map((entry) => entry.track.isLiked);
}

describe("Mock account ratings", () => {
  it("updates catalog, the current song, queue occurrences, and official suggestions from the same rating", () => {
    const fake = new FakeCoreClient();
    const deltas: StateDelta[] = [];
    const initial = fake.subscribeSnapshot((delta) => deltas.push(delta)).snapshot;

    expect(fake.sendMutation(like(FIRST, false), ACCOUNT)).toEqual({ kind: "setLike", outcome: "confirmed", error: null });
    expect(fake.sendMutation(like(OFFICIAL, false), ACCOUNT).outcome).toBe("confirmed");

    const home = fake.queryCatalog({ kind: "home" });
    const playlist = fake.queryCatalog({ kind: "playlistTracks" });
    const history = fake.queryCatalog({ kind: "recentHistory" });
    const search = fake.queryCatalog({ kind: "searchSongs", query: "Synthetic Fixture" });
    const album = fake.queryCatalog({ kind: "albumDetail", albumId: "mock-alb-1" });
    const artist = fake.queryCatalog({ kind: "artistDetail", artistId: "mock-art-1" });
    expect([
      home.resultType === "home" && home.recentTracks?.[0]?.isLiked,
      playlist.resultType === "playlistTracks" && playlist.tracks.items[0]?.isLiked,
      history.resultType === "recentHistory" && history.items[0]?.isLiked,
      search.resultType === "searchSongs" && search.songs.items[0]?.isLiked,
      album.resultType === "albumDetail" && album.tracks[0]?.isLiked,
      artist.resultType === "artistDetail" && artist.topSongs?.[0]?.isLiked,
    ]).toEqual([false, false, false, false, false, false]);

    const fresh = fake.subscribeSnapshot(() => {}).snapshot;
    expect(initial.player.currentTrack?.isLiked).toBe(true);
    expect(fresh.player.currentTrack?.isLiked).toBe(false);
    expect(queueLikes(fresh.queueSummary, FIRST)).toEqual([false, false]);
    expect(fresh.queueSummary.queueRevision).toBe(initial.queueSummary.queueRevision);
    expect(fresh.queueSummary.officialUpcoming?.tracks?.find((track) => track.videoId === OFFICIAL)?.isLiked).toBe(false);
    expect(deltas.map((event) => event.deltaType)).toEqual([
      "playerChanged", "queueReplaced", "resourceInvalidated", "queueReplaced", "resourceInvalidated",
    ]);
    expect(deltas.filter((delta) => delta.deltaType === "resourceInvalidated").map((delta) => delta.resource_kind))
      .toEqual(["likedSongs", "likedSongs"]);
  });

  it("keeps like pages ordered across ratings made before a cursor and each item marked liked", () => {
    const fake = new FakeCoreClient();
    const first = page(fake);
    expect(first.items.map(({ videoId }) => videoId)).toEqual([FIRST, "mock-vid-00005"]);
    expect(first.nextCursor).not.toBeNull();
    fake.sendMutation(like(FIRST, false), ACCOUNT);
    expect(page(fake, first.nextCursor).items.map(({ videoId, isLiked }) => [videoId, isLiked]))
      .toEqual([["mock-vid-00009", true], ["mock-vid-00013", true]]);
    fake.sendMutation(like(SECOND, true), ACCOUNT);
    expect(page(fake).items.map(({ videoId }) => videoId)).toEqual([SECOND, "mock-vid-00005"]);
  });

  it("requires a confirmation and the active online mock account without changing ratings on refusal", () => {
    const fake = new FakeCoreClient();
    const request = like(FIRST, false);
    const cases = [
      [fake.sendMutation({ ...request, confirmedByUser: false }, ACCOUNT), "INVALID_PAYLOAD"],
      [fake.sendMutation(request, "different-account"), "ACCOUNT_MISMATCH"],
    ];
    for (const [result, code] of cases) {
      expect(result).toMatchObject({ outcome: "rejected", error: { code } });
    }
    for (const [scenario, code] of [
      ["authExpired", "AUTH_EXPIRED"],
      ["offlineCached", "NETWORK_OFFLINE"],
      ["unsupported", "UNSUPPORTED_CAPABILITY"],
      ["empty", "UNSUPPORTED_CAPABILITY"],
      ["bridgeCrashed", "UNSUPPORTED_CAPABILITY"],
    ] as const) {
      fake.setScenario(scenario);
      expect(fake.subscribeSnapshot(() => {}).snapshot.capabilities.likeMutation).toBe("unsupported");
      expect(fake.sendMutation(request, ACCOUNT)).toMatchObject({ outcome: "rejected", error: { code } });
    }
    fake.setScenario("normal");
    expect(page(fake).items[0]?.videoId).toBe(FIRST);
  });
});

it("reports audio quality for the second synthetic song and restores the first song's unknown fields", () => {
  const fake = new FakeCoreClient();
  const player = () => fake.subscribeSnapshot(() => {}).snapshot.player;
  expect(player()).toMatchObject({ audioQuality: null, audioBitrateKbps: null, codec: "UNKNOWN (Mock)" });
  fake.sendPlayerCommand({ commandId: "next", op: "next" });
  expect(player()).toMatchObject({ currentTrack: { videoId: SECOND }, audioQuality: "high", audioBitrateKbps: 192, codec: "mp4a.40.2" });
  fake.sendPlayerCommand({ commandId: "previous", op: "previous" });
  expect(player()).toMatchObject({ currentTrack: { videoId: FIRST }, audioQuality: null, audioBitrateKbps: null, codec: "UNKNOWN (Mock)" });
});
