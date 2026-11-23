#!/usr/bin/env python3
"""Live check of the DJ prompts on the official page through a real MCP stdio client: one prompt
shortly before a track ends, delivered to a waiting call that receives progress notifications,
and a check-in from the heartbeat at the chatty level. Writes omit operation IDs and
preconditions. Plays a public album and leaves it paused; the account library is untouched."""

import argparse
import importlib.util
import os
from pathlib import Path
import queue
import subprocess
import time


spec = importlib.util.spec_from_file_location("live_read", Path(__file__).with_name("live-read.py"))
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)

CHECK_IN_BUDGET_S = 16 * 60


class Agent:
    def __init__(self, client):
        self.client = client
        self.identifier = 2

    def call(self, tool, arguments):
        self.identifier += 1
        reply = self.client.call(tool, arguments, self.identifier)
        return reply["structuredContent"]["value"], reply["isError"]

    def wait(self, seconds, **arguments):
        """Calls ytm_wait_dj_prompt with a progress token; returns the reply, progress count and
        how long the call took."""
        self.identifier += 1
        token = f"wait-{self.identifier}"
        started = time.monotonic()
        self.client.send(live.smoke.rpc("tools/call", {
            "name": "ytm_wait_dj_prompt", "arguments": {"maxWaitSeconds": seconds, **arguments},
            "_meta": {"progressToken": token}}, self.identifier))
        progress, deadline = 0, started + seconds + 30
        while (remaining := deadline - time.monotonic()) > 0:
            try:
                frame = self.client.frames.get(timeout=remaining)
            except queue.Empty:
                break
            if frame.get("method") == "notifications/progress" and frame["params"].get("progressToken") == token:
                progress += 1
            elif frame.get("id") == self.identifier:
                return frame["result"], progress, time.monotonic() - started
            else:
                self.client.unmatched.append(frame)
        raise TimeoutError("ytm_wait_dj_prompt did not return")

    def context(self):
        value, error = self.call("ytm_get_context", {"previewLimit": 5})
        return None if error else value


def track_of(snapshot):
    return (snapshot or {}).get("player", {}).get("track") or {}


def cue_summary(result, progress, took):
    value = result["structuredContent"]["value"]
    cue = value.get("cue")
    if cue is None:
        return dict(cue=None, progress=progress, tookS=round(took, 1))
    playing = cue.get("nowPlaying") or {}
    return dict(seq=cue["seq"], kind=cue["kind"], reasons=cue["reasons"], progress=progress,
                tookS=round(took, 1), videoId=(playing.get("track") or {}).get("videoId"),
                remainingMs=playing.get("remainingMs"), upNext=len(cue["upNext"]),
                recent=[(item["track"]["videoId"], item["outcome"]) for item in cue["recent"]],
                lastInteraction=cue["listener"]["lastInteraction"],
                promptInText=cue["prompt"] in result["content"][0]["text"])


def verify(client):
    agent = Agent(client)
    album = live.public_album_id(client, agent.identifier)
    if album is None:
        return dict(status="BLOCKED", reason="no verified public album")
    detail, error = agent.call("ytm_get_album", {"albumId": album, "page": {"limit": 6}})
    if error or len(detail["tracks"]) < 3:
        return dict(status="BLOCKED", reason="album did not return three tracks")
    record = dict(status="FAIL")
    started, error = agent.call("ytm_play_context", {"target": {"kind": "album", "albumId": album, "startIndex": 0}})
    record["start"] = dict(phase=started.get("phase"), code=(started.get("error") or {}).get("code") or started.get("code"),
                           autoOperationId=str(started.get("operationId", "")).startswith("auto-"))
    snapshot = None
    for _ in range(40):
        snapshot = agent.context()
        anchor = (snapshot or {}).get("player", {}).get("timeAnchor") or {}
        if snapshot and snapshot["player"]["playback"] == "playing" and anchor.get("durationMs"):
            break
        time.sleep(0.5)
    else:
        record["reason"] = "nothing played with a known duration"
        return record
    duration = snapshot["player"]["timeAnchor"]["durationMs"]
    first = track_of(snapshot).get("videoId")
    agent.wait(2, userSpoke=True, chattiness="chatty")

    seeked, _ = agent.call("ytm_seek", {"positionMs": max(duration - 100_000, 0)})
    record["seek"] = dict(phase=seeked.get("phase"), code=(seeked.get("error") or {}).get("code") or seeked.get("code"))
    ending = cue_summary(*agent.wait(150))
    ending["sameTrack"] = ending.get("videoId") == first
    record["trackEnding"] = ending

    cues, begun = [], time.monotonic()
    while time.monotonic() - begun < CHECK_IN_BUDGET_S:
        summary = cue_summary(*agent.wait(600))
        summary["atS"] = round(time.monotonic() - begun)
        if summary.get("kind"):
            cues.append(summary)
        if summary.get("kind") == "checkIn":
            break
    record["later"] = cues
    check_in = next((cue for cue in cues if cue["kind"] == "checkIn"), None)
    finished = any(video == first and outcome == "finished" for cue in cues for video, outcome in cue["recent"])
    paused, _ = agent.call("ytm_control_playback", {"action": "pause"})
    record["pause"] = paused.get("phase")
    passed = (ending.get("kind") == "trackEnding" and ending["sameTrack"] and ending["progress"] >= 1
              and ending["remainingMs"] is not None and ending["remainingMs"] <= 80_000
              and ending["promptInText"] and check_in is not None and finished)
    record["firstTrackFinished"] = finished
    record["status"] = "PASS_TARGET" if passed else "FAIL"
    return record


def run(mcp, env, scope):
    argv = [mcp, "register", "--principal", "candidate-dj-check", "--account-scope", scope,
            "--hours", "1", "--grant", "dj"]
    credential = Path(subprocess.run(argv, env=env, capture_output=True, text=True, timeout=5,
                                     check=True).stdout.strip().splitlines()[-1])
    client = live.smoke.init_mcp(mcp, credential, env)
    try:
        record = verify(client)
    finally:
        client.close()
    record["stage"] = "djPrompts"
    live.report(record)
    return 0 if record["status"] == "PASS_TARGET" else 2 if record["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-root", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    raise SystemExit(live.run_native_session(args.package_root.resolve(), args.evidence_dir.resolve(), run))
