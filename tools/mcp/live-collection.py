#!/usr/bin/env python3
"""Check that a public album plays its second track and continues on the official page."""

import argparse
import importlib.util
import os
from pathlib import Path
import time


spec = importlib.util.spec_from_file_location("live_read", Path(__file__).with_name("live-read.py"))
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)


def verify_album(client):
    identifier = 2
    album = live.public_album_id(client, identifier)
    if album is None:
        return dict(status="BLOCKED", reason="no verified public album")

    def call(tool, arguments):
        nonlocal identifier
        identifier += 1
        reply = client.call(tool, arguments, identifier)
        return reply["structuredContent"]["value"], reply["isError"]

    def receipt(operation_id):
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            value, error = client.ledger(operation_id)
            if error:
                return "receiptError", value.get("code")
            if value["schedulingStopped"]:
                return value["phase"], (value.get("error") or {}).get("code")
            time.sleep(0.2)
        return "pending", "TIMEOUT"

    detail, error = call("ytm_get_album", {"albumId": album, "page": {"limit": 3}})
    if error:
        return dict(status="FAIL", step="albumRead", code=detail.get("code"))
    tracks = detail["tracks"]
    if len(tracks) < 3 or tracks[1]["videoId"] == tracks[2]["videoId"]:
        return dict(status="BLOCKED", reason="album did not return distinct second and third tracks")

    for _ in range(30):
        before, error = call("ytm_get_context", {"previewLimit": 0})
        if not error and before["bridgeEpoch"] and before["player"]["connection"] == "ready":
            break
        time.sleep(0.2)
    else:
        return dict(status="BLOCKED", reason="official player did not become ready")

    # The official page restores and keeps its own queue across sessions; playing a single track
    # first gives it a fresh context, so the album below is loaded anew rather than jumped within.
    reset = "target-album-reset-context"
    outside, error = call("ytm_search", {"query": "instrumental piano", "limit": 5})
    album_videos = {track["videoId"] for track in tracks}
    reset_video = next((item["videoId"] for item in ([] if error else outside["items"])
                        if item["videoId"] not in album_videos), None)
    if reset_video is None:
        return dict(status="BLOCKED", reason="no reset track outside the album")
    call("ytm_play_context", {"operationId": reset, "preconditions": preconditions(before),
                              "target": {"kind": "video", "videoId": reset_video}})
    receipt(reset)
    for _ in range(30):
        before, error = call("ytm_get_context", {"previewLimit": 0})
        if not error and (before["player"].get("track") or {}).get("videoId") == reset_video:
            break
        time.sleep(0.3)
    # Let the page finish building the new context before switching to the album.
    time.sleep(3)
    before, error = call("ytm_get_context", {"previewLimit": 0})

    start = "target-album-start-index-one"
    submitted, error = call("ytm_play_context", {
        "operationId": start,
        "preconditions": preconditions(before),
        "target": {"kind": "album", "albumId": album, "startIndex": 1},
    })
    if error:
        return dict(status="FAIL", step="startRequest", code=submitted.get("code"))
    start_phase, start_error = receipt(start)
    second, error = call("ytm_get_context", {"previewLimit": 0})
    if error:
        return dict(status="FAIL", step="startSnapshot", code=second.get("code"), startPhase=start_phase)
    player = second["player"]
    native = player.get("playbackContext") or {}
    exact_track = (player.get("track") or {}).get("videoId") == tracks[1]["videoId"]
    result = dict(status="FAIL", startPhase=start_phase, startError=start_error,
                  secondTrackMatched=exact_track, secondNativeIndex=native.get("startIndex"),
                  playing=player["playback"] == "playing")
    if not (start_phase == "verified" and exact_track and native.get("playlistId")
            and native.get("startIndex") == 1 and result["playing"]):
        return result

    following = "target-album-next-track"
    next_preconditions = preconditions(second)
    next_preconditions["expectedQueueRevision"] = second["queue"]["revision"]
    submitted, error = call("ytm_control_playback", {
        "operationId": following, "preconditions": next_preconditions, "action": "next",
    })
    if error:
        return {**result, "nextRequestCode": submitted.get("code")}
    phase, code = receipt(following)
    # The receipt proves the track changed; give the page up to 10 s to settle on it and play.
    deadline = time.monotonic() + 10
    while True:
        third, error = call("ytm_get_context", {"previewLimit": 0})
        if error or time.monotonic() > deadline or (
                (third["player"].get("track") or {}).get("videoId") == tracks[2]["videoId"]
                and third["player"]["playback"] == "playing"):
            break
        time.sleep(0.5)
    if error:
        return {**result, "nextSnapshotCode": third.get("code"), "nextPhase": phase}
    result["observedThirdVideo"] = (third["player"].get("track") or {}).get("videoId")
    result["expectedThirdVideo"] = tracks[2]["videoId"]
    result["observedPlayback"] = third["player"]["playback"]
    next_player = third["player"]
    next_context = next_player.get("playbackContext") or {}
    matches = (next_player.get("track") or {}).get("videoId") == tracks[2]["videoId"]
    same_collection = next_context.get("playlistId") == native["playlistId"]
    playing = next_player["playback"] == "playing"
    result.update(nextPhase=phase, nextError=code, thirdTrackMatched=matches,
                  sameNativePlaylist=same_collection, thirdNativeIndex=next_context.get("startIndex"),
                  nextPlaying=playing)
    if phase == "verified" and matches and same_collection and next_context.get("startIndex") == 2 and playing:
        result["status"] = "PASS_TARGET"
    return result


def preconditions(snapshot):
    return {"expectedAccountEpoch": snapshot["account"]["accountEpoch"],
            "expectedBridgeEpoch": snapshot["bridgeEpoch"]}


def run(mcp, env, scope):
    credential = live.register(mcp, env, "candidate-album-playback", scope, playback=True)
    client = live.smoke.init_mcp(mcp, credential, env)
    client.ledger = live.operation_reader(credential)
    try:
        result = verify_album(client)
    except Exception as error:
        result = dict(status="FAIL", error=type(error).__name__)
    finally:
        client.close()
    live.report(dict(stage="albumPlayback", **result))
    return 0 if result["status"] == "PASS_TARGET" else 2 if result["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-root", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    raise SystemExit(live.run_native_session(args.package_root.resolve(), args.evidence_dir.resolve(), run))
