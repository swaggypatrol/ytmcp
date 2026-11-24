#!/usr/bin/env python3
"""
Reference background waiter for YTMcp Moment DJ.

Shows the host-side loop this example implements:
1. Keep at most ONE ytm_wait_dj_prompt open per principal.
2. Skip repeated prompts by `(coreEpoch, seq)`, since a restarted player counts `seq` from zero.
3. Stop when the player is gone instead of starting it again.

A real host also cancels the open wait when the listener speaks, handles the message, and passes
`userSpoke` / `spoke` on its next wait rather than opening a second one.

Usage (illustrative MCP JSON-RPC stdio client):
  python3 tools/mcp/dj-waiter-example.py --credential-file ~/.config/ytmcp/control-credentials/my-dj.json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


class McpStdioClient:
    def __init__(self, binary: str, credential: Path, allow_start: bool) -> None:
        args = [binary, "--credential-file", str(credential)]
        if allow_start:
            args.append("--allow-background-start")
        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr,
            text=True,
        )
        self._id = 0

    def request(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        assert self.proc.stdin and self.proc.stdout
        self._id += 1
        req_id = self._id
        frame = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            frame["params"] = params
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise EOFError("ytmcp closed stdio")
            msg = json.loads(line)
            if msg.get("id") == req_id:
                return msg

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        assert self.proc.stdin
        frame = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            frame["params"] = params
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        return self.request("tools/call", {"name": name, "arguments": arguments})

    def close(self) -> None:
        if self.proc.stdin:
            self.proc.stdin.close()
        self.proc.wait(timeout=5)


def run_waiter_loop(client: McpStdioClient, chattiness: str) -> None:
    client.request(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "ytmcp-dj-waiter-example", "version": "1.2.0"},
        },
    )
    client.notify("notifications/initialized")

    seen_cues: set[tuple[str | None, int]] = set()
    pending_user_spoke = False
    pending_spoke = False

    while True:
        reply = client.call_tool(
            "ytm_wait_dj_prompt",
            {
                "userSpoke": pending_user_spoke,
                "spoke": pending_spoke,
                "chattiness": chattiness,
                "maxWaitSeconds": 600,
            },
        )
        pending_user_spoke = False
        pending_spoke = False

        if reply.get("result", {}).get("isError"):
            err = reply["result"].get("structuredContent", {}).get("value", {})
            print(f"[dj-waiter] stopped: {err.get('code')} {err.get('message')}", file=sys.stderr)
            break

        value = reply["result"]["structuredContent"]["value"]
        cue = value.get("cue")
        if cue is None:
            continue

        cue_key = (cue.get("coreEpoch"), int(cue["seq"]))
        if cue_key in seen_cues:
            continue
        seen_cues.add(cue_key)

        moment = cue.get("moment") or {}
        print(
            f"[dj-waiter] cue {cue_key} kind={cue['kind']} "
            f"env={moment.get('environment', {}).get('status')} "
            f"intents={len(moment.get('listeningIntents', {}).get('items', []))}"
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="ytmcp", help="Path to ytmcp binary")
    parser.add_argument("--credential-file", type=Path, required=True)
    parser.add_argument("--chattiness", default="normal", choices=["off", "quiet", "normal", "chatty"])
    parser.add_argument("--allow-background-start", action="store_true")
    args = parser.parse_args()

    client = McpStdioClient(args.binary, args.credential_file, args.allow_background_start)
    try:
        run_waiter_loop(client, args.chattiness)
    except (KeyboardInterrupt, EOFError):
        pass
    finally:
        client.close()


if __name__ == "__main__":
    main()
