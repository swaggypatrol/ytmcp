#!/usr/bin/env python3
"""Acceptance run: Claude Code (headless) as the DJ of an extracted candidate for a long stretch
with nobody answering. Only ytm tools are allowed, so no permission prompt can appear. Records
whether the wait loop ever stopped, how many prompts and check-ins arrived, how often the agent
spoke, and any tool refusal. Every videoId the agent writes must have appeared in an earlier tool
result it received, and reads must carry their JSON data in the text the model sees. Plays a
public album; the account library is untouched.

Usage: live-agent-loop.py PACKAGE_ROOT EVIDENCE_DIR [MINUTES]  (default 15, about three songs)"""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location("live_dj", Path(__file__).with_name("live-dj.py"))
dj = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dj)
live = dj.live

INSTRUCTIONS = """You are my DJ for the next while. I'm away from the screen and will not reply.
Use the ytm tools. Call ytm_wait_dj_prompt in a loop and act on each prompt as you judge best:
queue songs that fit (find them with ytm_search), or do nothing. When you want to say something to me, just write it; I hear
your text. After writing, call ytm_wait_dj_prompt again right away and keep the loop going until
I stop you. Never end your turn on your own."""

def carries_json(text):
    """True when a read result's text is its JSON reply, alone or after a summary line."""
    for data in (text, text.partition("\n")[2]):
        try:
            if isinstance(json.loads(data), dict):
                return True
        except ValueError:
            pass
    return False


def result_text(content):
    """The text a tool result showed the model, whether the stream gives a string or blocks."""
    if isinstance(content, str):
        return content
    return "\n".join(block.get("text", "") for block in content or [] if isinstance(block, dict))


def written_video_ids(value):
    """Every videoId or videoIds value in a tool call's arguments."""
    if isinstance(value, dict):
        found = []
        for key, item in value.items():
            if key == "videoId" and isinstance(item, str):
                found.append(item)
            elif key == "videoIds" and isinstance(item, list):
                found.extend(entry for entry in item if isinstance(entry, str))
            else:
                found.extend(written_video_ids(item))
        return found
    if isinstance(value, list):
        return [entry for item in value for entry in written_video_ids(item)]
    return []


def run(mcp, env, scope, minutes):
    argv = [mcp, "register", "--principal", "claude-dj-loop", "--account-scope", scope,
            "--hours", "4", "--grant", "dj"]
    credential = Path(subprocess.run(argv, env=env, capture_output=True, text=True, timeout=5,
                                     check=True).stdout.strip().splitlines()[-1])
    client = live.smoke.init_mcp(mcp, credential, env)
    agent = dj.Agent(client)
    # Songs of three to four minutes, so the default run covers about three of them.
    found, _ = agent.call("ytm_search", {"query": "Rumours Fleetwood Mac", "kind": "album", "limit": 8})
    album = next((item["albumId"] for item in live.result_page({"result": "searchEntities", "value": found})["items"]
                  if item["kind"] == "album" and item["title"].strip().lower() == "rumours"), None)
    if album is None:
        album = live.public_album_id(client, agent.identifier)
    started, _ = agent.call("ytm_play_context", {"target": {"kind": "album", "albumId": album, "startIndex": 0}})
    evidence = Path(sys.argv[2])
    config = evidence / "claude-mcp.json"
    config.write_text(json.dumps({"mcpServers": {"ytm": {
        "command": str(mcp), "args": ["--credential-file", str(credential)]}}}))
    stream = evidence / "agent-stream.jsonl"
    began = time.monotonic()
    with stream.open("w") as out:
        process = subprocess.Popen(
            ["claude", "-p", INSTRUCTIONS, "--mcp-config", str(config), "--strict-mcp-config",
             "--allowedTools", "mcp__ytm__*", "--model", "sonnet", "--output-format", "stream-json",
             "--verbose"],
            stdout=out, stderr=subprocess.DEVNULL, cwd=evidence, text=True)
        try:
            exit_code = process.wait(timeout=minutes * 60)
            ended = "agentStopped"
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                process.kill()
            exit_code, ended = None, "timeLimit"
    elapsed = round((time.monotonic() - began) / 60, 1)
    paused, _ = agent.call("ytm_control_playback", {"action": "pause"})
    client.close()

    waits, prompts, check_ins, spoke_turns, refusals, gaps = 0, 0, [], 0, [], []
    last_wait = None
    calls = {}
    # Text of every tool result so far, what the agent wrote, and which reads showed their data.
    seen_text, written, ungrounded = "", [], []
    reads, reads_with_json, read_text = 0, 0, ""
    for line in stream.read_text().splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        message = event.get("message") or {}
        for block in message.get("content") or []:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                calls[block.get("id")] = block.get("name", "")
                if block.get("name", "").endswith("ytm_wait_dj_prompt"):
                    waits += 1
                for video in written_video_ids(block.get("input")):
                    written.append(video)
                    if video not in seen_text:
                        ungrounded.append(video)
            elif block.get("type") == "text" and event.get("type") == "assistant" and block.get("text", "").strip():
                spoke_turns += 1
            elif block.get("type") == "tool_result":
                name = calls.get(block.get("tool_use_id"), "")
                shown = result_text(block.get("content"))
                seen_text += "\n" + shown
                text = json.dumps(block.get("content"))
                if block.get("is_error"):
                    refusals.append((name, text[:200]))
                elif "__ytm_get_" in name or name.endswith("__ytm_search"):
                    reads += 1
                    if carries_json(shown):
                        reads_with_json += 1
                        read_text += "\n" + shown
                if name.endswith("ytm_wait_dj_prompt"):
                    if '"checkIn"' in text or "check in" in text:
                        check_ins.append(round((time.monotonic() - began) / 60, 1))
                    if "No DJ prompt yet" not in text:
                        prompts += 1
    record = dict(stage="agentLoop", minutesPlanned=minutes, minutesRan=elapsed, ended=ended,
                  claudeExit=exit_code, start=started.get("phase"), waitCalls=waits,
                  promptsReceived=prompts, checkInsReceived=len(check_ins), agentTextTurns=spoke_turns,
                  toolErrors=refusals[:10], pause=paused.get("phase"), readCalls=reads,
                  readCallsWithJson=reads_with_json, videoIdsWritten=len(written),
                  videoIdsWrittenFromReads=sum(video in read_text for video in written),
                  ungroundedVideoIds=ungrounded[:10])
    # An agent may stay quiet, but whatever it writes must come from results it was shown,
    # and its reads must have shown it their data as text.
    grounded = not ungrounded and (reads == 0 or reads_with_json > 0)
    record["status"] = "PASS_TARGET" if ended == "timeLimit" and waits > 0 and grounded else "FAIL"
    live.report(record)
    return 0 if record["status"] == "PASS_TARGET" else 1


if __name__ == "__main__":
    os.umask(0o077)
    minutes = int(sys.argv[3]) if len(sys.argv) > 3 else 15
    raise SystemExit(live.run_native_session(
        Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(),
        lambda mcp, env, scope: run(mcp, env, scope, minutes)))
