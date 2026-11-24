#!/usr/bin/env python3
"""
Terminal demo of `dj_harness`: a background DJ wait that a listener's typing interrupts.

Each cue is printed as it arrives. Every line typed on stdin counts as the listener speaking:
the open wait is cancelled, and the next one reports `userSpoke`. A real harness passes the line
to its model instead, and calls `resume(spoke=...)` after the model's turn.

Usage:
  python3 tools/mcp/dj-waiter-example.py --credential-file ~/.config/ytmcp/control-credentials/my-dj.json
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from dj_harness import DjWaiter, McpConnection  # noqa: E402


async def print_cues(inbox: asyncio.Queue) -> None:
    while True:
        delivered = await inbox.get()
        cue = delivered.cue
        print(f"[dj] cue {cue.get('coreEpoch')}/{cue['seq']} {cue['kind']}: {cue.get('prompt', '')[:120]}")


async def listener_lines(waiter: DjWaiter) -> None:
    loop = asyncio.get_running_loop()
    while line := await loop.run_in_executor(None, sys.stdin.readline):
        print(f"[listener] {line.strip()}")
        waiter.interrupt()
        waiter.resume(spoke=False)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="ytmcp", help="Path to the ytmcp binary")
    parser.add_argument("--credential-file", type=Path, required=True)
    parser.add_argument("--chattiness", default="normal", choices=["off", "quiet", "normal", "chatty"])
    args = parser.parse_args()

    connection = await McpConnection.spawn(args.binary, args.credential_file)
    await connection.initialize("ytmcp-dj-waiter-example", "demo")
    inbox: asyncio.Queue = asyncio.Queue()
    waiter = DjWaiter(connection, inbox, chattiness=args.chattiness)
    waiter.start()
    tasks = [asyncio.create_task(print_cues(inbox)), asyncio.create_task(listener_lines(waiter))]
    try:
        await connection.closed.wait()
    finally:
        await waiter.stop()
        for task in tasks:
            task.cancel()
        await connection.close()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
