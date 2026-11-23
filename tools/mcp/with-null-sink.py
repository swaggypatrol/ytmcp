#!/usr/bin/env python3
"""Run a live check with this app's audio on a temporary null sink.

Loads a null sink, starts the command with PULSE_SINK pointing at it, and while the command
runs moves any stream of the player that still lands on another sink onto the null sink.
Prints one JSON line with the guard result and exits with the command's status."""

import json
import subprocess
import sys
import threading
import time

SINK = "ytmcp_live_null"
PLAYER_BINARIES = {"ytmcp-player", "electron", "chrome"}


def pactl_json(*args):
    out = subprocess.run(["pactl", "-f", "json", *args], capture_output=True, text=True, timeout=5)
    return json.loads(out.stdout or "[]") if out.returncode == 0 else []


def player_streams():
    for stream in pactl_json("list", "sink-inputs"):
        props = stream.get("properties", {})
        binary = str(props.get("application.process.binary", "")).lower()
        if binary in PLAYER_BINARIES or "ytm" in binary:
            yield stream


def main():
    if len(sys.argv) < 2:
        print("usage: with-null-sink.py COMMAND...", file=sys.stderr)
        return 2
    module = subprocess.run(
        ["pactl", "load-module", "module-null-sink", f"sink_name={SINK}",
         "sink_properties=device.description=YTMGlassLiveNull"],
        capture_output=True, text=True, timeout=5)
    if module.returncode != 0:
        print(json.dumps({"stage": "audioGuard", "status": "BLOCKED", "reason": "null sink unavailable"}))
        return 2
    sink_index = next((s["index"] for s in pactl_json("list", "sinks") if s.get("name") == SINK), None)
    moved, seen, stop = [], set(), threading.Event()

    def guard():
        while not stop.is_set():
            for stream in player_streams():
                seen.add(stream["index"])
                if stream.get("sink") != sink_index:
                    subprocess.run(["pactl", "move-sink-input", str(stream["index"]), SINK],
                                   capture_output=True, timeout=5)
                    moved.append(stream["index"])
            stop.wait(0.1)

    watcher = threading.Thread(target=guard, daemon=True)
    watcher.start()
    env = dict(__import__("os").environ, PULSE_SINK=SINK)
    try:
        status = subprocess.run(sys.argv[1:], env=env).returncode
    finally:
        stop.set()
        watcher.join(timeout=2)
        subprocess.run(["pactl", "unload-module", module.stdout.strip()], capture_output=True, timeout=5)
    print(json.dumps({"stage": "audioGuard", "status": "PASS_TARGET" if not moved else "FAIL",
                      "playerStreams": len(seen), "movedToNullSink": len(set(moved))}), flush=True)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
