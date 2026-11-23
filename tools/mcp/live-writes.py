#!/usr/bin/env python3
"""Real-account write check limited to explicitly authorized targets: the private playlist
"YTM Glass Test" (append one public track, read it back, remove that exact occurrence, read
back) and a like on a public track confirmed neutral first, then restored to neutral."""

import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import time


spec = importlib.util.spec_from_file_location("live_read", Path(__file__).with_name("live-read.py"))
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)

TEST_PLAYLIST = "YTM Glass Test"  # account data, named before the rename


class Client:
    def __init__(self, mcp, credential, env):
        self.client = live.smoke.init_mcp(mcp, credential, env)
        self.ledger = live.operation_reader(credential)
        self.identifier = 2

    def call(self, tool, arguments):
        self.identifier += 1
        reply = self.client.call(tool, arguments, self.identifier)
        return reply["structuredContent"]["value"], reply["isError"]

    def close(self):
        self.client.close()

    def receipt(self, operation_id, seconds=40):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value, error = self.ledger(operation_id)
            if error:
                return dict(phase="receiptError", code=value.get("code"))
            if value["schedulingStopped"]:
                return value
            time.sleep(0.3)
        return dict(phase="pending", code="TIMEOUT")

    def submit(self, tool, operation_id, epoch, **arguments):
        value, error = self.call(tool, {"operationId": operation_id,
                                        "preconditions": {"expectedAccountEpoch": epoch}, **arguments})
        return dict(phase="requestError", code=value.get("code")) if error else self.receipt(operation_id)


def outcome(receipt):
    return dict(phase=receipt.get("phase"), code=(receipt.get("error") or {}).get("code") or receipt.get("code"))


def register(mcp, env, principal, scope, *grants):
    argv = [mcp, "register", "--principal", principal, "--account-scope", scope, "--hours", "1"]
    for grant in grants:
        argv.extend(grant.split(" "))
    process = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=5, check=True)
    return Path(process.stdout.strip().splitlines()[-1])


def find_test_playlist(client):
    matches, cursor = [], None
    for _ in range(10):
        page = {"limit": 100} | ({"cursor": cursor} if cursor else {})
        value, error = client.call("ytm_get_library", {"kind": "playlists", "page": page})
        if error:
            return None, "libraryReadFailed"
        items = value["page"]["items"]
        matches += [item["playlistId"] for item in items if item["title"].strip() == TEST_PLAYLIST]
        cursor = value["page"]["nextCursor"]
        if not cursor:
            break
    if len(matches) != 1:
        return None, f"expected exactly one '{TEST_PLAYLIST}', found {len(matches)}"
    return matches[0], None


def read_playlist(client, playlist_id):
    value, error = client.call("ytm_get_playlist", {"playlistId": playlist_id, "page": {"limit": 100}})
    return None if error else value


def run(mcp, env, scope):
    record = dict(stage="realAccountWrites", status="FAIL")
    lookup = Client(mcp, register(mcp, env, "candidate-write-lookup", scope), env)
    try:
        playlist_id, reason = find_test_playlist(lookup)
        if playlist_id is None:
            record.update(status="BLOCKED", reason=reason)
            return finish(record)
        before = read_playlist(lookup, playlist_id)
        if not before or before.get("editable") is not True or before.get("privacy") != "private":
            record.update(status="BLOCKED", reason="the test playlist is not a verified editable private playlist")
            return finish(record)
        album = live.public_album_id(lookup.client, 2)
        tracks, _ = lookup.call("ytm_get_album", {"albumId": album, "page": {"limit": 6}}) if album else ({}, True)
        videos = [track["videoId"] for track in tracks.get("tracks", [])]
        present = {entry["track"]["videoId"] for entry in before["entries"]["items"]}
        candidate = next((video for video in videos if video not in present), None)
        if not album or candidate is None or not before["entries"]["complete"]:
            record.update(status="BLOCKED", reason="no public track outside the fully read test playlist")
            return finish(record)
    finally:
        lookup.close()

    writer = Client(mcp, register(mcp, env, "candidate-write-check", scope,
                                  "--grant append-playlist", "--grant edit-playlist", "--grant rating",
                                  "--grant playback", f"--playlist {playlist_id}"), env)
    try:
        context, _ = writer.call("ytm_get_context", {"previewLimit": 0})
        epoch = context["account"]["accountEpoch"]
        added = writer.submit("ytm_add_playlist_entries", "live-write-append", epoch,
                              playlistId=playlist_id, videoIds=[candidate])
        record["append"] = outcome(added)
        entry = ((added.get("itemResults") or [{}])[0]).get("resourceEntryId")
        after_add = read_playlist(writer, playlist_id)
        record["appendReadBack"] = bool(after_add and any(
            item["entryId"] == entry and item["track"]["videoId"] == candidate
            for item in after_add["entries"]["items"]))
        # Whatever the receipt says, remove every new occurrence of the candidate by its ID so the
        # test playlist ends as it began, even after an unknown or partial append.
        previous = {item["entryId"] for item in before["entries"]["items"]}
        stray = [item["entryId"] for item in (after_add or {}).get("entries", {}).get("items", [])
                 if item["track"]["videoId"] == candidate and item["entryId"] not in previous]
        if entry and entry not in stray:
            stray.append(entry)
        if stray:
            removed = writer.submit("ytm_edit_playlist_entries", "live-write-remove", epoch,
                                    playlistId=playlist_id, edit={"action": "remove", "entryIds": stray})
            record["remove"] = outcome(removed)
        after_remove = read_playlist(writer, playlist_id)
        record["restored"] = bool(after_remove) and sorted(
            item["track"]["videoId"] for item in after_remove["entries"]["items"]) == sorted(
            item["track"]["videoId"] for item in before["entries"]["items"])

        record["rating"] = rating_check(writer, epoch, album, videos)
    finally:
        writer.close()
    passed = (record.get("append", {}).get("phase") == "verified" and record["appendReadBack"]
              and record.get("remove", {}).get("phase") == "verified" and record["restored"]
              and record["rating"].get("status") in {"PASS_TARGET", "BLOCKED"})
    record["status"] = "PASS_TARGET" if passed else "FAIL"
    return finish(record)


def rating_check(writer, epoch, album, videos):
    context, _ = writer.call("ytm_get_context", {"previewLimit": 0})
    started = writer.call("ytm_play_context", {
        "operationId": "live-write-rating-play",
        "preconditions": {"expectedAccountEpoch": epoch, "expectedBridgeEpoch": context["bridgeEpoch"]},
        "target": {"kind": "album", "albumId": album, "startIndex": 1}})
    if started[1] or writer.receipt("live-write-rating-play").get("phase") != "verified":
        return dict(status="BLOCKED", reason="could not play the rating candidate")
    track, error = writer.call("ytm_get_track", {"target": {"kind": "video", "videoId": videos[1]}})
    rating = None if error else track["track"]["rating"]
    fresh = not error and track.get("freshness") == "fresh" and track.get("ratingObservedAtMs")
    # Only a freshly read neutral rating on a track absent from the fully read Liked Music may be
    # changed; restoring a stale neutral would otherwise delete a real like.
    if rating != "neutral" or not fresh:
        return dict(status="BLOCKED", reason=f"candidate rating {rating} (fresh={bool(fresh)}), left untouched")
    in_liked = liked_contains(writer, videos[1])
    if in_liked is not False:
        return dict(status="BLOCKED", reason="Liked Music could not be read completely or contains the candidate")
    liked = writer.submit("ytm_set_rating", "live-write-like", epoch,
                          target={"kind": "video", "videoId": videos[1]}, rating="liked")
    after_like, _ = writer.call("ytm_get_track", {"target": {"kind": "video", "videoId": videos[1]}})
    restored = writer.submit("ytm_set_rating", "live-write-unlike", epoch,
                             target={"kind": "video", "videoId": videos[1]}, rating="neutral")
    final, _ = writer.call("ytm_get_track", {"target": {"kind": "video", "videoId": videos[1]}})
    result = dict(like=outcome(liked), likedReadBack=after_like.get("track", {}).get("rating"),
                  restore=outcome(restored), finalRating=final.get("track", {}).get("rating"))
    ok = (result["like"]["phase"] == "verified" and result["likedReadBack"] == "liked"
          and result["restore"]["phase"] == "verified" and result["finalRating"] == "neutral")
    result["status"] = "PASS_TARGET" if ok else "FAIL"
    return result


def liked_contains(client, video_id):
    cursor = None
    for _ in range(12):
        page = {"limit": 100} | ({"cursor": cursor} if cursor else {})
        value, error = client.call("ytm_get_library", {"kind": "liked", "page": page})
        if error:
            return None
        items = value["page"]["items"]
        if any(item.get("videoId") == video_id for item in items):
            return True
        cursor = value["page"]["nextCursor"]
        if not cursor:
            return False if value["page"]["complete"] else None
    return None


def finish(record):
    live.report(record)
    return 0 if record["status"] == "PASS_TARGET" else 2 if record["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-root", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    raise SystemExit(live.run_native_session(args.package_root.resolve(), args.evidence_dir.resolve(), run))
