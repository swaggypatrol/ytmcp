"""Fake-host tests for `dj_harness`: a scripted MCP server on a socket pair stands in for ytmcp.

Run: python3 -m unittest tools/mcp/test_dj_harness.py
"""

from __future__ import annotations

import asyncio
import json
import socket
import sys
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import dj_harness  # noqa: E402
from dj_harness import DjWaiter, McpConnection, SeenCues  # noqa: E402


class FakeServer:
    """Holds every `ytm_wait_dj_prompt` open until the test answers it; other tools reply at once.
    A cancelled wait gets no reply unless the test asks for a late one."""

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.reader = reader
        self.writer = writer
        self.waits: asyncio.Queue[tuple[int, dict[str, Any]]] = asyncio.Queue()
        self.open_waits: set[int] = set()
        self.max_open_waits = 0
        self.cancelled: asyncio.Queue[int] = asyncio.Queue()
        self.task = asyncio.create_task(self.serve())

    async def serve(self) -> None:
        while line := await self.reader.readline():
            message = json.loads(line)
            method = message.get("method")
            if method == "notifications/cancelled":
                request_id = message["params"]["requestId"]
                self.open_waits.discard(request_id)
                await self.cancelled.put(request_id)
            elif method == "tools/call":
                name = message["params"]["name"]
                if name == dj_harness.WAIT_TOOL:
                    self.open_waits.add(message["id"])
                    self.max_open_waits = max(self.max_open_waits, len(self.open_waits))
                    await self.waits.put((message["id"], message["params"]["arguments"]))
                else:
                    await self.reply(message["id"], {"structuredContent": {"type": "context", "value": {"tool": name}}})
            elif "id" in message:
                await self.reply(message["id"], {})

    async def reply(self, request_id: int, result: dict[str, Any]) -> None:
        self.open_waits.discard(request_id)
        self.writer.write((json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n").encode())
        await self.writer.drain()

    async def answer_wait(self, request_id: int, cue: dict[str, Any] | None) -> None:
        await self.reply(request_id, {"structuredContent": {"type": "djCue", "value": {"cue": cue}}})

    async def fail_wait(self, request_id: int, code: str) -> None:
        await self.reply(
            request_id,
            {"isError": True, "structuredContent": {"type": "error", "value": {"code": code, "message": "x"}}},
        )

    async def next_wait(self) -> tuple[int, dict[str, Any]]:
        return await asyncio.wait_for(self.waits.get(), 2)


def cue(seq: int, epoch: str = "core-a") -> dict[str, Any]:
    return {"seq": seq, "coreEpoch": epoch, "kind": "trackEnding", "ageMs": 0}


class DjWaiterTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        client_sock, server_sock = socket.socketpair()
        reader, writer = await asyncio.open_connection(sock=client_sock)
        server_reader, server_writer = await asyncio.open_connection(sock=server_sock)
        self.server = FakeServer(server_reader, server_writer)
        self.conn = McpConnection(reader, writer)
        self.conn.start()
        self.inbox: asyncio.Queue = asyncio.Queue()
        self.waiter = DjWaiter(self.conn, self.inbox, max_wait_seconds=600)

    async def asyncTearDown(self) -> None:
        await self.waiter.stop()
        await self.conn.close()
        self.server.writer.close()
        self.server.task.cancel()
        await asyncio.gather(self.server.task, return_exceptions=True)

    async def test_a_foreground_call_completes_while_the_wait_is_open(self) -> None:
        self.waiter.start()
        wait_id, _ = await self.server.next_wait()
        result = await asyncio.wait_for(self.conn.call_tool("ytm_search", {"query": "x"}), 1)
        self.assertEqual(result["structuredContent"]["value"]["tool"], "ytm_search")
        self.assertIn(wait_id, self.server.open_waits)
        await self.server.answer_wait(wait_id, cue(1))
        delivered = await asyncio.wait_for(self.inbox.get(), 1)
        self.assertEqual(delivered.cue["seq"], 1)

    async def test_the_listener_speaking_cancels_the_wait_and_the_next_one_reports_it(self) -> None:
        self.waiter.start()
        first, arguments = await self.server.next_wait()
        self.assertFalse(arguments["userSpoke"])
        self.waiter.interrupt()
        self.assertEqual(await asyncio.wait_for(self.server.cancelled.get(), 1), first)
        # No second wait while the host handles the listener.
        await asyncio.sleep(dj_harness.CANCEL_GRACE_S + 0.2)
        self.assertTrue(self.server.waits.empty())
        self.waiter.resume(spoke=True)
        second, arguments = await self.server.next_wait()
        self.assertNotEqual(second, first)
        self.assertTrue(arguments["userSpoke"])
        self.assertTrue(arguments["spoke"])
        self.assertEqual(self.server.max_open_waits, 1)

    async def test_a_cue_already_on_its_way_when_cancelled_is_still_delivered(self) -> None:
        self.waiter.start()
        first, _ = await self.server.next_wait()
        self.waiter.interrupt()
        await asyncio.wait_for(self.server.cancelled.get(), 1)
        await self.server.answer_wait(first, cue(7))
        delivered = await asyncio.wait_for(self.inbox.get(), 1)
        self.assertEqual(delivered.cue["seq"], 7)
        self.waiter.resume()
        await self.server.next_wait()
        self.assertEqual(self.server.max_open_waits, 1)

    async def test_stopping_inside_the_cancel_grace_returns_and_opens_no_wait(self) -> None:
        for resumed in (False, True):
            with self.subTest(resumed=resumed):
                waiter = DjWaiter(self.conn, self.inbox, max_wait_seconds=600)
                waiter.start()
                first, _ = await self.server.next_wait()
                waiter.interrupt()
                self.assertEqual(await asyncio.wait_for(self.server.cancelled.get(), 1), first)
                if resumed:
                    waiter.resume()
                await asyncio.sleep(0.1)
                await asyncio.wait_for(waiter.stop(), 1)
                await asyncio.sleep(0.1)
                self.assertTrue(self.server.waits.empty())
                self.assertEqual(self.server.open_waits, set())

    async def test_no_wait_opens_before_the_hosts_turn_is_done(self) -> None:
        self.waiter.start()
        request_id, _ = await self.server.next_wait()
        await self.server.fail_wait(request_id, dj_harness.PLAYER_GONE)
        await asyncio.sleep(0.1)
        # The listener speaks to a parked waiter, and the host's call starts the player mid-turn.
        self.waiter.interrupt()
        self.waiter.player_back()
        await asyncio.sleep(0.1)
        self.assertTrue(self.server.waits.empty())
        # The host's next turn starts before the waiter gets to run.
        self.waiter.resume(spoke=True)
        self.waiter.interrupt()
        await asyncio.sleep(0.1)
        self.assertTrue(self.server.waits.empty())
        self.waiter.resume()
        _, arguments = await self.server.next_wait()
        self.assertTrue(arguments["userSpoke"])
        self.assertTrue(arguments["spoke"])
        self.assertEqual(self.server.max_open_waits, 1)

    async def test_a_wait_after_a_cancelled_one_names_the_last_cue_received(self) -> None:
        self.waiter.start()
        first, arguments = await self.server.next_wait()
        self.assertNotIn("lastReceived", arguments)
        await self.server.answer_wait(first, cue(4))
        second, arguments = await self.server.next_wait()
        self.assertEqual(arguments["lastReceived"], {"coreEpoch": "core-a", "seq": 4})
        # The cancelled wait never answers, as the real server does; its cue is offered again
        # only because the next wait says which one arrived last.
        self.waiter.interrupt()
        self.assertEqual(await asyncio.wait_for(self.server.cancelled.get(), 1), second)
        self.waiter.resume()
        _, arguments = await asyncio.wait_for(self.server.waits.get(), dj_harness.CANCEL_GRACE_S + 1)
        self.assertEqual(arguments["lastReceived"], {"coreEpoch": "core-a", "seq": 4})

    async def test_repeated_cues_are_dropped_but_a_restarted_player_counts_again(self) -> None:
        self.waiter.start()
        for item in (cue(1), cue(1), cue(1, epoch="core-b")):
            request_id, _ = await self.server.next_wait()
            await self.server.answer_wait(request_id, item)
        await self.server.next_wait()
        seen = [self.inbox.get_nowait().cue for _ in range(self.inbox.qsize())]
        self.assertEqual([(c["coreEpoch"], c["seq"]) for c in seen], [("core-a", 1), ("core-b", 1)])

    async def test_a_player_that_quit_parks_the_waiter_instead_of_restarting_it(self) -> None:
        self.waiter.start()
        request_id, _ = await self.server.next_wait()
        await self.server.fail_wait(request_id, dj_harness.PLAYER_GONE)
        await asyncio.sleep(0.2)
        self.assertEqual(self.waiter.parked_reason, "the player is not running")
        self.assertTrue(self.server.waits.empty())
        self.waiter.player_back()
        await self.server.next_wait()
        self.assertIsNone(self.waiter.parked_reason)


    async def test_a_parked_waiter_tries_again_after_the_hosts_turn(self) -> None:
        self.waiter.start()
        request_id, _ = await self.server.next_wait()
        await self.server.fail_wait(request_id, dj_harness.PLAYER_GONE)
        await asyncio.sleep(0.1)
        # The listener speaks after the player quit, and the host never calls `player_back()`.
        self.waiter.interrupt()
        self.waiter.resume(spoke=True)
        _, arguments = await self.server.next_wait()
        self.assertTrue(arguments["userSpoke"])
        self.assertTrue(arguments["spoke"])
        self.assertIsNone(self.waiter.parked_reason)

class SeenCuesTests(unittest.TestCase):
    def test_the_cache_is_bounded(self) -> None:
        seen = SeenCues(limit=2)
        self.assertTrue(seen.first_time(cue(1)))
        self.assertTrue(seen.first_time(cue(2)))
        self.assertTrue(seen.first_time(cue(3)))
        # The oldest key fell out; the newer ones are still known.
        self.assertTrue(seen.first_time(cue(1)))
        self.assertFalse(seen.first_time(cue(3)))


if __name__ == "__main__":
    unittest.main()
