#!/usr/bin/env python3
"""Real-harness check: Claude Code (headless, `claude -p`) as the DJ client of an extracted
candidate. Starts an album with this script's own MCP client, then asks Claude Code to search,
seek near the end, wait for the DJ prompt and report what it received. The report must repeat a
search result's videoId, which only reaches the model through an ordinary read result. Uses the caller's Claude Code login
and plays a public album; the account library is untouched.

Usage: live-agent-dj.py PACKAGE_ROOT EVIDENCE_DIR"""
import importlib.util, json, os, subprocess, sys, time
from pathlib import Path
spec = importlib.util.spec_from_file_location("live_dj", Path(__file__).with_name("live-dj.py"))
dj = importlib.util.module_from_spec(spec); spec.loader.exec_module(dj)
live = dj.live

# Unrelated to the album being played, so a search result cannot be the playing track.
SEARCH_QUERY = "Dreams Fleetwood Mac"

PROMPT = f"""You are checking a music player's DJ prompts through the ytm MCP server. Do exactly this:
1. Call ytm_search with query "{SEARCH_QUERY}" and limit 5, and note the videoId of the first result.
2. Call ytm_get_context and note the current track's durationMs from player.timeAnchor.
3. Call ytm_seek with positionMs equal to durationMs minus 70000 (omit every other field).
4. Call ytm_wait_dj_prompt with maxWaitSeconds 150.
5. Reply with only one JSON object, no prose: {{"searchVideoId": <from step 1>, "durationMs": <from step 2>, "kind": ..., "videoId": ..., "title": ..., "remainingMs": ..., "promptStart": <first 12 words of the prompt>, "sawRecentAndUpNext": <true if the result listed upNext and recent>}}.
Do not call any other tool."""

def run(mcp, env, scope):
    argv = [mcp, "register", "--principal", "claude-code-dj", "--account-scope", scope, "--hours", "1",
            "--grant", "dj"]
    credential = Path(subprocess.run(argv, env=env, capture_output=True, text=True, timeout=5, check=True).stdout.strip().splitlines()[-1])
    client = live.smoke.init_mcp(mcp, credential, env)
    agent = dj.Agent(client)
    album = live.public_album_id(client, agent.identifier)
    started, _ = agent.call("ytm_play_context", {"target": {"kind": "album", "albumId": album, "startIndex": 2}})
    for _ in range(40):
        snap = agent.context()
        if snap and snap["player"]["playback"] == "playing" and snap["player"]["timeAnchor"].get("durationMs"):
            break
        time.sleep(0.5)
    expected = dj.track_of(snap).get("videoId")
    duration = ((snap or {}).get("player", {}).get("timeAnchor") or {}).get("durationMs")
    found, _ = agent.call("ytm_search", {"query": SEARCH_QUERY, "limit": 5})
    search_ids = {item["videoId"] for item in found.get("items", [])} - {expected}
    config = Path(sys.argv[2]) / "claude-mcp.json"
    config.write_text(json.dumps({"mcpServers": {"ytm": {"command": str(mcp), "args": ["--credential-file", str(credential)], "env": {k: env[k] for k in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME") if k in env}}}}))
    tools = ",".join(f"mcp__ytm__{name}" for name in ("ytm_search", "ytm_get_context", "ytm_seek",
                                                       "ytm_wait_dj_prompt"))
    out = subprocess.run(["claude", "-p", PROMPT, "--mcp-config", str(config), "--strict-mcp-config",
                          "--allowedTools", tools, "--model", "sonnet", "--output-format", "json"],
                         capture_output=True, text=True, timeout=400, cwd=sys.argv[2])
    result = {}
    try:
        reply = json.loads(out.stdout)
        text = reply.get("result", "")
        result = json.loads(text[text.index("{"):text.rindex("}") + 1])
    except Exception as error:
        result = dict(parseError=type(error).__name__, stdoutTail=out.stdout[-300:], stderrTail=out.stderr[-300:])
    paused, _ = agent.call("ytm_control_playback", {"action": "pause"})
    client.close()
    # The search videoId exists only in a read result, never in this prompt; the duration match is
    # recorded only, since the seek already proves the context reached the model.
    read_data_reached_model = result.get("searchVideoId") in search_ids
    ok = (read_data_reached_model and result.get("kind") == "trackEnding" and result.get("videoId") == expected
          and (result.get("remainingMs") or 10**9) <= 80000)
    record = dict(stage="claudeCodeDj", status="PASS_TARGET" if ok else "FAIL", start=started.get("phase"),
                  expectedVideo=expected, searchVideoIds=sorted(search_ids),
                  readDataReachedModel=read_data_reached_model,
                  durationMatched=duration is not None and result.get("durationMs") == duration,
                  agentReport=result, claudeExit=out.returncode, pause=paused.get("phase"))
    live.report(record)
    return 0 if ok else 1

os.umask(0o077)
raise SystemExit(live.run_native_session(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), run))
