"""Host-side adapter that lets an agent harness be a background DJ over `ytmcp` stdio.

A harness that chats or plays with the listener keeps its own event loop. This module gives it:

* `McpConnection`: one JSON-RPC stdio connection with a single read pump. Replies are routed by
  request id and progress by token, so foreground tool calls (search, play, like) run while a
  long `ytm_wait_dj_prompt` is open.
* `DjWaiter`: keeps at most one background wait open and puts each new cue in the host's inbox.
  When the listener speaks, `interrupt()` cancels the wait and the next one reports `userSpoke`;
  no wait is open from then until the host calls `resume(spoke=...)` once its turn is done. A cue
  that was already on its way when the wait was cancelled is still delivered, and no second wait
  opens until the first has ended. Each wait names the last cue received (`lastReceived`), so a
  cue the cancelled wait took but never answered is offered again. Cues are deduplicated by
  `(coreEpoch, seq)` in a bounded cache. When the player is gone (the listener quit it) the
  waiter parks instead of starting it again; `player_back()` resumes it, for example after a
  foreground call the listener asked for started the player (still not before `resume()`).

The host decides what to do with a cue. A cue that waited in the inbox can be stale: read
`ytm_get_context` before acting on it (`DjWaiter.deliver_age_ms` says how long it waited).

Usage:

    conn = await McpConnection.spawn("ytmcp", credential_path)
    await conn.initialize("my-harness", "1.0")
    inbox: asyncio.Queue = asyncio.Queue()
    waiter = DjWaiter(conn, inbox)
    waiter.start()
    # listener says something:   waiter.interrupt()
    # host finished its turn:     waiter.resume(spoke=True)
    # cue = await inbox.get()     -> DeliveredCue(cue=..., received_at=...)
"""

from __future__ import annotations

import asyncio
import itertools
import json
import time
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

PROTOCOL_VERSION = "2025-06-18"
WAIT_TOOL = "ytm_wait_dj_prompt"
# How long a cancelled wait may still answer before the next one opens anyway.
CANCEL_GRACE_S = 2.0
# Pause before retrying a wait that failed for a reason other than a missing player.
RETRY_DELAY_S = 5.0
PLAYER_GONE = "UNAVAILABLE"


class McpError(Exception):
    """A JSON-RPC error reply."""

    def __init__(self, error: dict[str, Any]) -> None:
        super().__init__(error.get("message", "MCP error"))
        self.error = error


class McpConnection:
    """One MCP stdio connection; a single task reads every frame and routes it."""

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self._reader = reader
        self._writer = writer
        self._ids = itertools.count(1)
        self._pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self._progress: dict[int, Callable[[dict[str, Any]], None]] = {}
        self._pump: asyncio.Task[None] | None = None
        self._process: asyncio.subprocess.Process | None = None
        self.closed = asyncio.Event()

    @classmethod
    async def spawn(cls, binary: str, credential: Path, allow_start: bool = False) -> "McpConnection":
        """Starts `ytmcp`. Without `allow_start` no call ever starts the player."""
        args = [binary, "--credential-file", str(credential)]
        if allow_start:
            args.append("--allow-background-start")
        process = await asyncio.create_subprocess_exec(
            *args, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE
        )
        assert process.stdout and process.stdin
        connection = cls(process.stdout, process.stdin)  # type: ignore[arg-type]
        connection._process = process
        connection.start()
        return connection

    def start(self) -> None:
        if self._pump is None:
            self._pump = asyncio.create_task(self._read_pump())

    async def initialize(self, name: str, version: str) -> dict[str, Any]:
        result = await self.call(
            "initialize",
            {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": name, "version": version}},
        )
        await self.notify("notifications/initialized")
        return result

    async def send_request(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        on_progress: Callable[[dict[str, Any]], None] | None = None,
    ) -> tuple[int, asyncio.Future[dict[str, Any]]]:
        """Sends a request and returns its id and a future for the reply's `result`."""
        request_id = next(self._ids)
        future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        if self.closed.is_set():
            future.set_exception(EOFError("ytmcp closed stdio"))
            return request_id, future
        self._pending[request_id] = future
        params = dict(params or {})
        if on_progress is not None:
            params["_meta"] = {"progressToken": request_id}
            self._progress[request_id] = on_progress
        await self._write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return request_id, future

    async def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        _, future = await self.send_request(method, params)
        return await future

    async def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        return await self.call("tools/call", {"name": name, "arguments": arguments})

    async def cancel(self, request_id: int, reason: str) -> None:
        """Asks the server to stop a request. Its reply, if one is already on the way, still
        resolves the request's future; the caller decides how long to wait for it."""
        await self.notify("notifications/cancelled", {"requestId": request_id, "reason": reason})

    def forget(self, request_id: int) -> None:
        """Stops waiting for a request's reply (after a cancellation got no answer)."""
        future = self._pending.pop(request_id, None)
        self._progress.pop(request_id, None)
        if future is not None and not future.done():
            future.cancel()

    async def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        frame: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            frame["params"] = params
        await self._write(frame)

    async def close(self) -> None:
        if not self._writer.is_closing():
            self._writer.close()
        if self._process is not None:
            try:
                await asyncio.wait_for(self._process.wait(), 5)
            except asyncio.TimeoutError:
                self._process.terminate()
        if self._pump is not None:
            await asyncio.gather(self._pump, return_exceptions=True)

    async def _write(self, frame: dict[str, Any]) -> None:
        self._writer.write((json.dumps(frame) + "\n").encode())
        await self._writer.drain()

    async def _read_pump(self) -> None:
        try:
            while line := await self._reader.readline():
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                await self._route(message)
        finally:
            self.closed.set()
            for future in self._pending.values():
                if not future.done():
                    future.set_exception(EOFError("ytmcp closed stdio"))
            self._pending.clear()
            self._progress.clear()

    async def _route(self, message: dict[str, Any]) -> None:
        if "method" in message:
            if message["method"] == "notifications/progress":
                token = message.get("params", {}).get("progressToken")
                handler = self._progress.get(token) if isinstance(token, int) else None
                if handler is not None:
                    handler(message["params"])
            elif "id" in message and message["method"] == "ping":
                await self._write({"jsonrpc": "2.0", "id": message["id"], "result": {}})
            return
        request_id = message.get("id")
        future = self._pending.pop(request_id, None) if isinstance(request_id, int) else None
        self._progress.pop(request_id, None)  # type: ignore[arg-type]
        if future is None or future.done():
            return
        if "error" in message:
            future.set_exception(McpError(message["error"]))
        else:
            future.set_result(message.get("result", {}))


@dataclass(frozen=True)
class DeliveredCue:
    cue: dict[str, Any]
    received_at: float


class SeenCues:
    """Bounded `(coreEpoch, seq)` cache: a restarted player counts `seq` from zero again."""

    def __init__(self, limit: int = 256) -> None:
        self._limit = limit
        self._seen: OrderedDict[tuple[str | None, int], None] = OrderedDict()

    def first_time(self, cue: dict[str, Any]) -> bool:
        key = (cue.get("coreEpoch"), int(cue["seq"]))
        if key in self._seen:
            self._seen.move_to_end(key)
            return False
        self._seen[key] = None
        if len(self._seen) > self._limit:
            self._seen.popitem(last=False)
        return True


async def _first(future: "asyncio.Future[Any]", event: asyncio.Event, timeout: float | None = None) -> None:
    """Returns once `future` is done or `event` is set, or after `timeout` seconds."""
    signal = asyncio.create_task(event.wait())
    try:
        await asyncio.wait({future, signal}, timeout=timeout, return_when=asyncio.FIRST_COMPLETED)
    finally:
        signal.cancel()


class DjWaiter:
    """Keeps one background DJ wait open and hands new cues to `inbox`."""

    def __init__(
        self,
        connection: McpConnection,
        inbox: "asyncio.Queue[DeliveredCue]",
        chattiness: str = "normal",
        max_wait_seconds: int = 600,
        seen_limit: int = 256,
    ) -> None:
        self._connection = connection
        self._inbox = inbox
        self._chattiness = chattiness
        self._max_wait = max_wait_seconds
        self._seen = SeenCues(seen_limit)
        # Something `_run` waits on changed: the host's turn, parking or stopping.
        self._wake = asyncio.Event()
        self._interrupted = asyncio.Event()
        self._stopped = asyncio.Event()
        self._turn_open = False
        self._user_spoke = False
        self._spoke = False
        self._last_received: dict[str, Any] | None = None
        self._task: asyncio.Task[None] | None = None
        self.parked_reason: str | None = None

    def start(self) -> None:
        if self._task is None:
            self._stopped.clear()
            self._task = asyncio.create_task(self._run())

    async def stop(self) -> None:
        """Cancels the open wait without a grace and ends the waiter; no wait opens after this."""
        self._stopped.set()
        self._interrupted.set()
        self._wake.set()
        if self._task is not None:
            await asyncio.gather(self._task, return_exceptions=True)
            self._task = None

    def interrupt(self) -> None:
        """The listener spoke: cancel the open wait, wait for `resume()`, and report it then."""
        self._user_spoke = True
        self._turn_open = True
        self._interrupted.set()

    def resume(self, spoke: bool = False) -> None:
        """The host's turn is done; `spoke` says whether it talked to the listener. A parked waiter
        tries again: the wait only attaches, so it finds a relaunched player and never starts one."""
        self._spoke = self._spoke or spoke
        self._turn_open = False
        self.parked_reason = None
        self._wake.set()

    def player_back(self) -> None:
        """The player is running again (for example a foreground call started it)."""
        if self.parked_reason is not None:
            self.parked_reason = None
            self._wake.set()

    @staticmethod
    def deliver_age_ms(delivered: DeliveredCue) -> int:
        """How long a cue has waited since it arrived, plus its age at delivery."""
        waited = int((time.monotonic() - delivered.received_at) * 1000)
        return waited + int(delivered.cue.get("ageMs", 0))

    async def _run(self) -> None:
        while not self._stopped.is_set():
            if self._turn_open or self.parked_reason is not None:
                self._wake.clear()
                await self._wake.wait()
                continue
            self._interrupted.clear()
            arguments = {
                "userSpoke": self._user_spoke,
                "spoke": self._spoke,
                "chattiness": self._chattiness,
                "maxWaitSeconds": self._max_wait,
            }
            if self._last_received is not None:
                arguments["lastReceived"] = self._last_received
            self._user_spoke = self._spoke = False
            request_id, future = await self._connection.send_request(
                "tools/call", {"name": WAIT_TOOL, "arguments": arguments}, on_progress=lambda _: None
            )
            try:
                result = await self._settle(request_id, future)
            except EOFError:
                self.parked_reason = "ytmcp closed"
                return
            if result is None:
                continue
            outcome = self._handle(result)
            if outcome == "gone":
                self.parked_reason = "the player is not running"
            elif outcome == "error":
                try:
                    await asyncio.wait_for(self._stopped.wait(), RETRY_DELAY_S)
                except asyncio.TimeoutError:
                    pass

    async def _settle(self, request_id: int, future: "asyncio.Future[dict[str, Any]]") -> dict[str, Any] | None:
        """Waits for the reply; after a cancellation, waits a short grace for a reply that was
        already on its way, so no second wait overlaps the first. A stop cancels at once."""
        await _first(future, self._interrupted)
        if future.done():
            return future.result()
        stopping = self._stopped.is_set()
        try:
            await self._connection.cancel(request_id, "the host stopped the DJ" if stopping else "the listener spoke")
        except (ConnectionError, RuntimeError):
            pass
        if not stopping:
            await _first(future, self._stopped, CANCEL_GRACE_S)
            if future.done():
                return future.result()
        self._connection.forget(request_id)
        return None

    def _handle(self, result: dict[str, Any]) -> str:
        content = result.get("structuredContent") or {}
        value = content.get("value") or {}
        if result.get("isError"):
            return "gone" if value.get("code") == PLAYER_GONE else "error"
        cue = value.get("cue")
        if cue is not None and self._seen.first_time(cue):
            self._inbox.put_nowait(DeliveredCue(cue=cue, received_at=time.monotonic()))
            if isinstance(cue.get("coreEpoch"), str):
                self._last_received = {"coreEpoch": cue["coreEpoch"], "seq": int(cue["seq"])}
        return "ok"
