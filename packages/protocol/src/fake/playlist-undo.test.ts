import { afterEach, describe, expect, it, vi } from "vitest";
import type { MutationRequest, StateDelta } from "../generated/types.js";
import { FakeCoreClient } from "./index.js";

const ACCOUNT = "mock-account-epoch-1";
const PLAYLIST = "mock-pl-late-night";
const NEW_SONG = "mock-vid-00100";

function add(videoId: string): MutationRequest {
  return { kind: "addToPlaylist", videoId, playlistId: PLAYLIST, confirmedByUser: true };
}

function undo(videoId: string, undoToken: string): MutationRequest {
  return { kind: "undoAddToPlaylist", videoId, playlistId: PLAYLIST, undoToken, confirmedByUser: true };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("Mock playlist additions", () => {
  it("adds one entry with an undo token, refuses a duplicate, and undoes exactly once", () => {
    const fake = new FakeCoreClient();
    const deltas: StateDelta[] = [];
    fake.subscribeSnapshot((delta) => deltas.push(delta));
    expect(fake.subscribeSnapshot(() => {}).snapshot.capabilities.playlistAddMutation).toBe("supported");

    const added = fake.sendMutation(add(NEW_SONG), ACCOUNT);
    expect(added).toMatchObject({ kind: "addToPlaylist", outcome: "confirmed" });
    const token = added.undoToken!;
    expect(token).toBeTruthy();
    // The first 42 synthetic songs are already in the playlist: no second copy, no token.
    const duplicate = fake.sendMutation(add("mock-vid-00001"), ACCOUNT);
    expect(duplicate).toMatchObject({ outcome: "rejected", error: { code: "UNAVAILABLE_TRACK" } });
    expect(duplicate.undoToken).toBeUndefined();

    expect(fake.sendMutation(undo(NEW_SONG, token), ACCOUNT)).toEqual({ kind: "undoAddToPlaylist", outcome: "confirmed", error: null });
    expect(fake.sendMutation(undo(NEW_SONG, token), ACCOUNT)).toMatchObject({ outcome: "rejected", error: { code: "STALE_CONTEXT" } });
    expect(deltas.filter((delta) => delta.deltaType === "resourceInvalidated").map((delta) => [delta.resource_kind, delta.resource_id]))
      .toEqual([["playlistTracks", PLAYLIST], ["playlistTracks", PLAYLIST]]);
  });

  it("reports an entry removed elsewhere, refuses an expired token, and stays off where adds are unsupported", () => {
    vi.useFakeTimers();
    const fake = new FakeCoreClient();
    const first = fake.sendMutation(add(NEW_SONG), ACCOUNT).undoToken!;
    fake.simulateRemovalElsewhere(PLAYLIST, NEW_SONG);
    expect(fake.sendMutation(undo(NEW_SONG, first), ACCOUNT)).toMatchObject({ outcome: "rejected", alreadyRemoved: true });

    const second = fake.sendMutation(add(NEW_SONG), ACCOUNT).undoToken!;
    expect(fake.sendMutation(undo("mock-vid-00101", second), ACCOUNT)).toMatchObject({ error: { code: "INVALID_PAYLOAD" } });
    vi.advanceTimersByTime(FakeCoreClient.UNDO_LIFETIME_MS);
    expect(fake.sendMutation(undo(NEW_SONG, second), ACCOUNT)).toMatchObject({ outcome: "rejected", error: { code: "STALE_CONTEXT" } });

    fake.setScenario("unsupported");
    expect(fake.sendMutation(add("mock-vid-00102"), ACCOUNT)).toMatchObject({ error: { code: "UNSUPPORTED_CAPABILITY" } });
  });
});

describe("Mock liked songs playback context", () => {
  it("aligns each row with its native entry across pages, and leaves it out where unsupported", () => {
    const fake = new FakeCoreClient();
    const first = fake.queryCatalog({ kind: "likedSongs", limit: 2 });
    if (first.resultType !== "likedSongs") throw new Error("Expected mock liked songs");
    expect(first.playback).toEqual({ playlistId: "LM", entries: [{ startIndex: 0, playlistEntryId: null }, { startIndex: 1, playlistEntryId: null }] });
    const next = fake.queryCatalog({ kind: "likedSongs", limit: 2, cursor: first.nextCursor });
    if (next.resultType !== "likedSongs") throw new Error("Expected mock liked songs");
    expect(next.playback?.entries.map((entry) => entry.startIndex)).toEqual([2, 3]);

    fake.setScenario("unsupported");
    const plain = fake.queryCatalog({ kind: "likedSongs", limit: 2 });
    expect(plain.resultType === "likedSongs" && plain.playback).toBeUndefined();
  });
});
