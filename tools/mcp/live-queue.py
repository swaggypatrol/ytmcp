#!/usr/bin/env python3
"""Exercise queue writes on the official page: append with an autoplay tail, exact removal,
an interruption that resumes the interrupted track at its position, and an upcoming
replacement. Only the page's own play queue changes; the account library is untouched."""

import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import time


spec = importlib.util.spec_from_file_location("live_read", Path(__file__).with_name("live-read.py"))
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)

SAVED_POSITION_MS = 20_000


class Session:
    def __init__(self, client):
        self.client = client
        self.ledger = client.ledger
        self.identifier = 2

    def call(self, tool, arguments):
        self.identifier += 1
        reply = self.client.call(tool, arguments, self.identifier)
        return reply["structuredContent"]["value"], reply["isError"]

    def context(self):
        value, error = self.call("ytm_get_context", {"previewLimit": 20})
        return None if error else value

    def receipt(self, operation_id, done=lambda value: value["schedulingStopped"], seconds=30):
        deadline = time.monotonic() + seconds
        value = None
        while time.monotonic() < deadline:
            value, error = self.ledger(operation_id)
            if error:
                return dict(phase="receiptError", code=value.get("code"))
            if done(value):
                return value
            time.sleep(0.25)
        return dict(phase="pending", code="TIMEOUT", last=value and value.get("phase"))

    def write(self, tool, operation_id, snapshot, queue=True, **arguments):
        preconditions = {"expectedAccountEpoch": snapshot["account"]["accountEpoch"],
                         "expectedBridgeEpoch": snapshot["bridgeEpoch"]}
        if queue:
            preconditions["expectedQueueRevision"] = snapshot["queue"]["revision"]
        value, error = self.call(tool, {"operationId": operation_id, "preconditions": preconditions,
                                        **arguments})
        if error:
            return dict(phase="requestError", code=value.get("code"))
        return None


def summary(receipt):
    error = receipt.get("error") or {}
    return dict(phase=receipt.get("phase"), code=error.get("code") or receipt.get("code"))


def current_video(snapshot):
    return ((snapshot or {}).get("player", {}).get("track") or {}).get("videoId")


def position_ms(snapshot):
    anchor = (snapshot or {}).get("player", {}).get("timeAnchor") or {}
    return anchor.get("positionMs")


def wait_for(session, predicate, seconds=20):
    deadline = time.monotonic() + seconds
    snapshot = None
    while time.monotonic() < deadline:
        snapshot = session.context()
        if snapshot and predicate(snapshot):
            return snapshot
        time.sleep(0.3)
    return snapshot


def reset_queue(session, snapshot, video_id, operation_id):
    failed = session.write("ytm_play_context", operation_id, snapshot, queue=False,
                           target={"kind": "video", "videoId": video_id})
    result = failed or session.receipt(operation_id)
    wait_for(session, lambda s: current_video(s) == video_id, seconds=10)
    # Let the page finish building the new context before the next navigation.
    time.sleep(3)
    return result


def verify_queue(client):
    session = Session(client)
    album = live.public_album_id(client, session.identifier)
    if album is None:
        return dict(status="BLOCKED", reason="no verified public album")
    detail, error = session.call("ytm_get_album", {"albumId": album, "page": {"limit": 6}})
    tracks = [] if error else detail["tracks"]
    videos = [track["videoId"] for track in tracks]
    if len(set(videos)) < 5:
        return dict(status="BLOCKED", reason="album did not return five distinct tracks")
    ready = wait_for(session, lambda s: s["bridgeEpoch"] and s["player"]["connection"] == "ready")
    if not ready or not ready.get("bridgeEpoch"):
        return dict(status="BLOCKED", reason="official player did not become ready")
    record = dict(status="FAIL")

    # The official page keeps its queue across sessions; start from a fresh single-track context
    # outside the album, so the album below is loaded anew rather than jumped within.
    outside, error = session.call("ytm_search", {"query": "instrumental piano", "limit": 5})
    reset_video = next((item["videoId"] for item in ([] if error else outside["items"])
                        if item["videoId"] not in videos), None)
    if reset_video is None:
        return dict(status="BLOCKED", reason="no reset track outside the album")
    reset_queue(session, ready, reset_video, "live-queue-reset-before")
    ready = session.context()
    failed = session.write("ytm_play_context", "live-queue-start", ready, queue=False,
                           target={"kind": "album", "albumId": album, "startIndex": 1})
    start = failed or session.receipt("live-queue-start")
    record["start"] = summary(start)
    playing = wait_for(session, lambda s: current_video(s) == videos[1] and s["player"]["playback"] == "playing")
    if start.get("phase") != "verified" or current_video(playing) != videos[1]:
        record["startState"] = dict(current=current_video(playing), expected=videos[1],
                                    resetVideo=reset_video,
                                    playback=(playing or {}).get("player", {}).get("playback"),
                                    context=(playing or {}).get("player", {}).get("playbackContext"))
        return record
    queue = playing["queue"]
    record["queueShape"] = dict(completeness=queue.get("completeness"), hasMore=queue.get("hasMore"))

    # 1. Append at the end of the explicit album list, before any autoplay suggestions.
    failed = session.write("ytm_enqueue", "live-queue-append", playing, contextToken=playing["contextToken"],
                           mode="last", videoIds=[videos[4]])
    append = failed or session.receipt("live-queue-append")
    appended = ((append.get("itemResults") or [{}])[0]).get("resourceEntryId")
    record["append"] = dict(summary(append), status="PASS_TARGET" if append.get("phase") == "verified" else "FAIL")

    # 2. Interrupt at a known position and let the core replay the interrupted track from there.
    before_seek = session.context()
    failed = session.write("ytm_seek", "live-queue-seek-saved", before_seek, queue=False,
                           contextToken=before_seek["contextToken"], positionMs=SAVED_POSITION_MS)
    record["seekSaved"] = summary(failed or session.receipt("live-queue-seek-saved"))
    interrupted = session.context()
    failed = session.write("ytm_interrupt_with", "live-queue-interrupt", interrupted,
                           contextToken=interrupted["contextToken"], videoIds=[videos[3]],
                           resumePolicy="resumeInterrupted")
    waiting = failed or session.receipt(
        "live-queue-interrupt", done=lambda v: v["schedulingStopped"] or v["phase"] == "acknowledged")
    resource = waiting.get("resource") or {}
    interrupt = dict(summary(waiting), resume=resource.get("resume"),
                     savedPositionMs=resource.get("resumePositionMs"), status="FAIL")
    inserted = wait_for(session, lambda s: current_video(s) == videos[3])
    interrupt["interruptionPlaying"] = current_video(inserted) == videos[3]
    if waiting.get("phase") == "acknowledged" and interrupt["interruptionPlaying"]:
        duration = ((inserted["player"].get("track") or {}).get("durationMs")
                    or (inserted["player"].get("timeAnchor") or {}).get("durationMs"))
        if duration and duration > 10_000:
            failed = session.write("ytm_seek", "live-queue-seek-end", inserted, queue=False,
                                   contextToken=inserted["contextToken"], positionMs=duration - 4_000)
            interrupt["seekToEnd"] = summary(failed or session.receipt("live-queue-seek-end"))
        resumed = session.receipt("live-queue-interrupt", seconds=60)
        resource = resumed.get("resource") or {}
        interrupt["resume"] = dict(summary(resumed), resume=resource.get("resume"))
        back = wait_for(session, lambda s: current_video(s) == videos[1], seconds=15)
        interrupt["resumedTrackMatched"] = current_video(back) == videos[1]
        interrupt["resumedPositionMs"] = position_ms(back)
        if (interrupt["resume"]["phase"] == "verified" and interrupt["resume"]["resume"] == "resumed"
                and interrupt["resumedTrackMatched"]
                and (interrupt["resumedPositionMs"] or 0) >= SAVED_POSITION_MS - 1_500):
            interrupt["status"] = "PASS_TARGET"
    record["interrupt"] = interrupt

    # 3. Exact edits need a finite queue: the page refuses them while an autoplay tail exists.
    # Turn queue autoplay off for these steps and restore the original setting afterwards.
    finite = session.context()
    original_autoplay = finite["player"].get("autoplay")
    record["autoplayBefore"] = original_autoplay
    if original_autoplay is True:
        failed = session.write("ytm_set_playback_mode", "live-queue-autoplay-off", finite,
                               change={"kind": "autoplay", "enabled": False})
        record["autoplayOff"] = summary(failed or session.receipt("live-queue-autoplay-off"))
        finite = wait_for(session, lambda s: s["queue"].get("completeness") == "complete", seconds=10)
    record["finiteQueueShape"] = dict(completeness=finite["queue"].get("completeness"),
                                      hasMore=finite["queue"].get("hasMore"))
    remove = dict(status="BLOCKED", reason="no verified appended occurrence")
    if appended:
        failed = session.write("ytm_edit_queue", "live-queue-remove", finite, contextToken=finite["contextToken"],
                               change={"action": "remove", "entryId": appended})
        removed = failed or session.receipt("live-queue-remove")
        remaining = session.context()
        gone = all(entry["entryId"] != appended for entry in remaining["queue"]["entries"])
        remove = dict(summary(removed), occurrenceGone=gone,
                      status="PASS_TARGET" if removed.get("phase") == "verified" and gone else "FAIL")
    record["remove"] = remove

    replace_from = session.context()
    failed = session.write("ytm_edit_queue", "live-queue-replace", replace_from,
                           contextToken=replace_from["contextToken"],
                           change={"action": "replaceUpcoming", "videoIds": [videos[0], videos[2]]})
    replaced = failed or session.receipt("live-queue-replace")
    replace = dict(summary(replaced), status="FAIL")
    if replaced.get("phase") == "rejected" and replace["code"] == "CAPABILITY_UNAVAILABLE":
        # The page refuses to clear while its native queue still holds a continuation token,
        # which real album queues keep even with autoplay off.
        replace.update(status="BLOCKED", reason="the official queue keeps a continuation token")
    if replaced.get("phase") == "verified":
        after = session.context()
        entries = after["queue"]["entries"]
        current = next((i for i, entry in enumerate(entries)
                        if entry["entryId"] == after["queue"]["currentEntryId"]), None)
        replace["order"] = (current is not None and [entry["track"]["videoId"]
                            for entry in entries[current + 1:]] == [videos[0], videos[2]])
        replace["status"] = "PASS_TARGET" if replace["order"] else "FAIL"
    record["replace"] = replace

    if original_autoplay is True:
        restore_from = session.context()
        failed = session.write("ytm_set_playback_mode", "live-queue-autoplay-restore", restore_from,
                               change={"kind": "autoplay", "enabled": True})
        restored = failed or session.receipt("live-queue-autoplay-restore")
        record["autoplayRestored"] = dict(summary(restored),
                                          observed=(session.context() or {}).get("player", {}).get("autoplay"))

    # Leave the page with a plain single-track queue, paused, instead of the edited test queue.
    final = session.context()
    reset_queue(session, final, reset_video, "live-queue-reset-after")
    paused = session.context()
    failed = session.write("ytm_control_playback", "live-queue-pause", paused, queue=False, action="pause")
    record["leftPaused"] = summary(failed or session.receipt("live-queue-pause"))

    scenarios = [record[name]["status"] for name in ("append", "interrupt", "remove", "replace")]
    record["status"] = "PASS_TARGET" if all(state == "PASS_TARGET" for state in scenarios) else (
        "FAIL" if "FAIL" in scenarios else "BLOCKED")
    return record


def run(mcp, env, scope):
    argv = [mcp, "register", "--principal", "candidate-queue-writes", "--account-scope", scope,
            "--hours", "1", "--grant", "playback", "--grant", "queue"]
    process = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=5, check=True)
    credential = Path(process.stdout.strip().splitlines()[-1])
    client = live.smoke.init_mcp(mcp, credential, env)
    client.ledger = live.operation_reader(credential)
    try:
        result = verify_queue(client)
    except Exception as error:
        result = dict(status="FAIL", error=type(error).__name__)
    finally:
        client.close()
    live.report(dict(stage="queueWrites", **result))
    return 0 if result["status"] == "PASS_TARGET" else 2 if result["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-root", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    raise SystemExit(live.run_native_session(args.package_root.resolve(), args.evidence_dir.resolve(), run))
