#!/usr/bin/env python3
"""MCP stdio and local control integration with a single daemon and a synthetic host."""

import argparse
from collections import deque
import hashlib
import json
import os
from pathlib import Path
import queue
import secrets
import socket
import stat
import subprocess
import tempfile
import threading
import time

# Writes reply once settled, which can take the adapter up to 12 s.
TIMEOUT = 20


def write_json(path, data):
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "w") as file:
        json.dump(data, file)


class Process:
    def __init__(self, command, env):
        self.child = subprocess.Popen(command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      text=True, encoding="utf-8")
        self.frames = queue.Queue()
        self.unmatched = deque()
        threading.Thread(target=self.collect, daemon=True).start()

    def collect(self):
        for line in self.child.stdout:
            try:
                self.frames.put(json.loads(line))
            except ValueError as exc:
                self.frames.put(exc)
                break

    def send(self, frame):
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()

    def receive(self, predicate):
        for frame in list(self.unmatched):
            if predicate(frame):
                self.unmatched.remove(frame)
                return frame
        deadline = time.monotonic() + TIMEOUT
        while (remaining := deadline - time.monotonic()) > 0:
            frame = self.frames.get(timeout=remaining)
            if isinstance(frame, Exception):
                raise AssertionError("stdout contained a non-protocol line") from frame
            if predicate(frame):
                return frame
            self.unmatched.append(frame)
        raise TimeoutError("expected protocol frame did not arrive")

    def call(self, name, arguments, identifier):
        self.start(name, arguments, identifier)
        return self.result(identifier)

    def start(self, name, arguments, identifier):
        """Sends a call whose reply comes later; writes reply once their operation settles."""
        self.send(rpc("tools/call", {"name": name, "arguments": arguments}, identifier))

    def result(self, identifier):
        return self.receive(lambda frame: frame.get("id") == identifier)["result"]

    def close(self):
        self.child.stdin.close()
        assert self.child.wait(timeout=TIMEOUT) == 0


class LocalConnection:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(TIMEOUT)
        self.socket.connect(str(path))
        self.reader = self.socket.makefile("rb")

    def send(self, frame):
        self.socket.sendall((json.dumps(frame) + "\n").encode())

    def receive(self):
        return json.loads(self.reader.readline())

    def request(self, method, params):
        self.send({"controlVersion": 1, "requestId": 1, "request": {"method": method, "params": params}})
        return self.receive()["response"]

    def hello(self, token, principal="offline-fixture"):
        self.send({"controlVersion": 1, "principal": principal, "token": token})
        return self.receive()

    def subscribe(self):
        self.send({"controlVersion": 1, "requestId": 1, "request": {"method": "subscribe", "params": {}}})
        assert self.receive()["response"]["result"] == "context"

    def until_event(self, kind):
        while True:
            event = self.receive()["event"]
            yield event
            if event["event"] == kind:
                return

    def close(self):
        self.reader.close()
        self.socket.close()


class LossyRelay:
    """A second control socket that relays to the daemon but can mishandle the next submit:
    "drop" loses its reply, "swallow" never delivers it, "stale" refuses it as stale."""

    def __init__(self, runtime, upstream):
        directory = runtime / "ytmcp"
        directory.mkdir(mode=0o700)
        path = directory / "control.sock"
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(path))
        os.chmod(path, 0o600)
        self.listener.listen()
        self.upstream = upstream
        self.next_submit = None
        self.submits = 0
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self):
        while True:
            try:
                client, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self.relay, args=(client,), daemon=True).start()

    def relay(self, client):
        with client, socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as daemon:
            daemon.connect(str(self.upstream))
            down, up = client.makefile("rb"), daemon.makefile("rb")
            daemon.sendall(down.readline())
            client.sendall(up.readline())
            line = down.readline()
            if not line:
                return
            mode = None
            if json.loads(line)["request"]["method"] == "submit":
                self.submits += 1
                mode, self.next_submit = self.next_submit, None
            if mode == "swallow":
                return
            if mode == "stale":
                client.sendall((json.dumps({"type": "response", "requestId": 1, "response": {
                    "result": "error", "value": {"code": "STALE_CONTEXT", "retryDisposition": "not_retryable",
                                                 "message": "synthetic stale refusal", "retryAfterMs": None},
                }}) + "\n").encode())
                return
            daemon.sendall(line)
            reply = up.readline()
            if mode != "drop":
                client.sendall(reply)

    def close(self):
        self.listener.close()


def host(kind, seq, payload, core=None, bridge=None, request=None):
    return {
        "protocolVersion": 1, "kind": kind, "seq": seq, "coreEpoch": core,
        "bridgeEpoch": bridge, "requestId": request, "payload": payload,
    }


def rpc(method, params=None, identifier=None):
    frame = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        frame["params"] = params
    if identifier is not None:
        frame["id"] = identifier
    return frame


def init_mcp(binary, config, env, profile="development"):
    mcp = Process([binary, "--socket", "auto", "--credential-file", config, "--profile", profile], env)
    mcp.send(rpc("initialize", {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "ytm-offline-smoke", "version": "1"},
    }, 1))
    init_result = mcp.receive(lambda frame: frame.get("id") == 1)["result"]
    assert init_result["capabilities"]["tools"] is not None
    assert init_result["serverInfo"]["name"] == "ytmcp", init_result["serverInfo"]
    assert init_result["serverInfo"]["version"] == "1.0.0", init_result["serverInfo"]
    mcp.send(rpc("notifications/initialized"))
    return mcp


def run(daemon_bin, mcp_bin, root):
    runtime = root / "runtime"
    runtime.mkdir(mode=0o700)
    config_dir = root / "config" / "ytmcp"
    config_dir.mkdir(mode=0o700, parents=True)
    registrations = config_dir / "control-clients.json"
    credential = root / "credential.json"
    token = secrets.token_hex(32)
    registration = {
        "principal": "offline-fixture", "tokenSha256": hashlib.sha256(token.encode()).hexdigest(),
        "accountScope": None, "expiresAtMs": int(time.time() * 1000) + 60_000,
        "actions": ["playback", "queue"],
    }
    observer_token = secrets.token_hex(32)
    write_json(registrations, [registration, {
        **registration, "principal": "offline-observer", "actions": [],
        "tokenSha256": hashlib.sha256(observer_token.encode()).hexdigest(),
    }])
    write_json(credential, {"principal": "offline-fixture", "token": token})
    env = {**os.environ, "YTMCP_NO_MPRIS": "1", "XDG_RUNTIME_DIR": str(runtime),
           "YTMCP_DATA_DIR": str(root)}
    path = runtime / "ytmcp" / "control.sock"

    daemon = Process([daemon_bin], env)
    try:
        daemon.send(host("host.hello", 1, {
            "appVersion": "1.0.0", "electronVersion": "fixture", "chromiumVersion": "fixture",
            "networkState": "offline", "mockMode": True, "hostHealth": {
                "waylandNative": None, "gpuAccelerated": None, "sandboxEnabled": True,
                "secretStoreBackend": "unknown", "safeStorageAvailable": False, "trayAvailable": False,
                "mprisActive": False, "coreRestartCount": 0,
            },
        }, request="host-fixture"))
        epoch = daemon.receive(lambda frame: frame["kind"] == "core.ready")["coreEpoch"]
        assert stat.S_IMODE(path.stat().st_mode) == 0o600

        deny = LocalConnection(path)
        assert deny.hello("0" * 64)["error"]["code"] == "PERMISSION_DENIED"
        deny.close()
        print("grant validation passed")
        delta_clients = [LocalConnection(path), LocalConnection(path)]
        try:
            for client in delta_clients:
                assert client.hello(token)["error"] is None
                client.subscribe()

            daemon.send(host("ui.subscribeSnapshot", 2, {}, core=epoch, request="ui-fixture"))
            snap = daemon.receive(lambda frame: frame["kind"] == "core.snapshot")
            bridge = "fixture-bridge"
            caps = dict(snap["payload"]["capabilities"], playPause="supported", next="supported",
                        queueMirror="supported", queueEnqueue="supported", queueReorder="supported",
                        queueRemove="supported", queueAutoplay="supported")
            daemon.send(host("bridge.ready", 1, {
                "bridgeEpoch": bridge, "bridgeVersion": "fixture", "capabilities": caps,
                "loggedInObserved": False,
            }, core=epoch, bridge=bridge))
            daemon.receive(lambda frame: frame["kind"] == "player.snapshotRequest")
            current_track = {"videoId": "fixture-video", "title": "Synthetic song", "availability": "available",
                             "cover": {"remoteUrl": "https://synthetic.invalid/internal-only"}}
            player = {
                "bridgeEpoch": bridge, "playback": "playing", "trackInstanceId": "fixture-instance",
                "currentQueueEntryId": "entry-now", "positionMs": 56, "durationMs": 120_000,
                "playbackRate": 1, "sampleAgeMs": 0, "volume": 1, "muted": False, "currentTrack": current_track,
                "autoplay": False, "codec": "mp4a.40.2", "qualityLabel": "auto", "audioMode": "song",
            }
            daemon.send(host("bridge.playbackObserved", 2, player, core=epoch, bridge=bridge))
            for client in delta_clients:
                event = list(client.until_event("trackChanged"))[-1]
                assert event["videoId"] == "fixture-video"
            daemon.receive(lambda frame: frame["kind"] == "core.delta"
                           and frame["payload"].get("deltaType") == "playerChanged"
                           and (frame["payload"]["player"].get("currentTrack") or {}).get("videoId") == "fixture-video")
            print("both socket subscribers and the UI received the same track")
        finally:
            for client in delta_clients:
                client.close()

        items = [{"entryId": "entry-now", "videoId": "fixture-video", "track": current_track,
                  "availability": "available"}]
        def send_queue(seq, revision):
            daemon.send(host("bridge.queueObserved", seq, {
                "bridgeEpoch": bridge, "queueRevision": revision, "currentEntryId": "entry-now",
                "entries": items, "loadedRange": {"start": 0, "endExclusive": len(items)},
                "completeness": "complete", "hasMore": False,
            }, core=epoch, bridge=bridge))
        send_queue(3, 1)
        daemon.receive(lambda frame: frame["kind"] == "core.delta"
                       and frame["payload"].get("deltaType") == "queueReplaced")
        own = LocalConnection(path)
        observer = LocalConnection(path)
        assert own.hello(token)["error"] is None
        assert observer.hello(observer_token, "offline-observer")["error"] is None
        own.subscribe()
        observer.subscribe()
        playing_video = "fixture-video"
        for index in range(2):
            mcp = init_mcp(mcp_bin, credential, env, "development" if index == 0 else "common")
            try:
                mcp.send(rpc("tools/list", {}, 2))
                tools = mcp.receive(lambda frame: frame.get("id") == 2)["result"]["tools"]
                names = [tool["name"] for tool in tools]
                assert names == sorted(names) and "ytm_get_context" in names
                # The operation ledger stays inside the adapter.
                ledger = {"ytm_get_operation", "ytm_cancel_operation", "ytm_resume_operation"}
                assert not ledger & set(names) and len(names) == 39, names
                annotations = {tool["name"]: tool["annotations"] for tool in tools}
                assert annotations["ytm_get_context"]["openWorldHint"] is False
                if index == 0:
                    assert annotations["ytm_get_home"]["openWorldHint"] is True
                    assert annotations["ytm_set_artist_subscription"]["readOnlyHint"] is False
                    assert annotations["ytm_set_library_membership"]["readOnlyHint"] is False
                    assert annotations["ytm_set_library_membership"]["openWorldHint"] is True
                else:
                    # The old common profile is accepted and publishes every tool.
                    assert names == published, names
                published = names
                enqueue = next(tool for tool in tools if tool["name"] == "ytm_enqueue")["inputSchema"]
                assert not {"operationId", "preconditions", "contextToken"} & set(enqueue["required"])
                play = next(tool for tool in tools if tool["name"] == "ytm_play_context")["inputSchema"]
                entry = next(variant for variant in play["definitions"]["PlayContextTarget"]["oneOf"]
                             if variant["properties"]["kind"].get("const") == "queueEntry")
                assert entry["required"] == ["kind", "entryId"], entry
                mcp.send(rpc("tools/call", {"name": "ytm_get_context", "arguments": {}}, 3))
                result = mcp.receive(lambda frame: frame.get("id") == 3)["result"]
                assert result["structuredContent"]["value"]["player"]["track"]["videoId"] == playing_video
                assert "https://synthetic.invalid" not in str(result)
                if index == 0:
                    denied = mcp.call("ytm_set_library_membership", {
                        "operationId": "no-membership-grant", "preconditions": {},
                        "target": {"kind": "song", "id": "fixture-video"}, "saved": True,
                    }, 70)
                    assert denied["isError"] and denied["structuredContent"]["value"]["code"] == "PERMISSION_DENIED"
                    denied_text = denied["content"][0]["text"]
                    assert denied_text.startswith("Not done") and "record" in denied_text, denied_text
                    assert "libraryMembership" not in denied_text, denied_text
                    for hidden, arguments in (("ytm_get_operation", {"operationId": "queue-three"}),
                                              ("ytm_cancel_operation", {"targetOperationId": "queue-three"})):
                        mcp.send(rpc("tools/call", {"name": hidden, "arguments": arguments}, 71))
                        response = mcp.receive(lambda frame: frame.get("id") == 71)
                        assert "error" in response, response
                    audio = mcp.call("ytm_get_player", {"include": "audio"}, 9)["structuredContent"]["value"]
                    assert audio["observed"] is None and audio["pending"] is None
                    assert audio["audio"] == {"codec": "mp4a.40.2", "audioMode": "song", "qualityLabel": "auto", "drmProtected": None}
                    mcp.send(rpc("tools/call", {"name": "ytm_get_context",
                                              "arguments": {"confirmedByUser": True}}, 4))
                    response = mcp.receive(lambda frame: frame.get("id") == 4)
                    assert "error" in response or response["result"]["isError"]
                    snapshot = result["structuredContent"]["value"]
                    operation = {"operationId": "queue-three", "preconditions": {
                        "expectedBridgeEpoch": bridge, "expectedQueueRevision": 1,
                    }, "contextToken": snapshot["contextToken"], "mode": "next", "videoIds": ["A", "B", "C"]}
                    mcp.start("ytm_enqueue", operation, 5)
                    for offset, expected in enumerate(("C", "B", "A"), start=1):
                        frame = daemon.receive(lambda frame: frame["kind"] == "player.command"
                                               and frame["payload"]["op"] == "enqueue")
                        assert frame["payload"]["args"]["target"]["videoId"] == expected
                        items.insert(1, {"entryId": "entry-" + expected, "videoId": expected,
                                         "track": {"videoId": expected, "title": expected, "availability": "available"},
                                         "availability": "available"})
                        send_queue(3 + offset, 1 + offset)
                    receipt = mcp.result(5)["structuredContent"]["value"]
                    assert receipt["phase"] == "verified", receipt
                    assert [item["resourceEntryId"] for item in receipt["itemResults"]] == ["entry-A", "entry-B", "entry-C"]
                    assert [item["videoId"] for item in items] == ["fixture-video", "A", "B", "C"]
                    replay = mcp.call("ytm_enqueue", operation, 6)["structuredContent"]["value"]
                    assert replay["phase"] == "verified" and replay["revision"] == receipt["revision"]
                    conflict = mcp.call("ytm_enqueue", {**operation, "mode": "last"}, 7)["structuredContent"]["value"]
                    assert conflict["code"] == "IDEMPOTENCY_CONFLICT", conflict

                    for offset, (op, change, expected_order) in enumerate((
                        ("moveQueueEntry", {"action": "move", "entryId": "entry-C", "beforeEntryId": "entry-A"},
                         ["entry-now", "entry-C", "entry-A", "entry-B"]),
                        ("removeQueueEntry", {"action": "remove", "entryId": "entry-B"},
                         ["entry-now", "entry-C", "entry-A"]),
                    )):
                        edit_context = mcp.call("ytm_get_context", {}, 20 + offset)["structuredContent"]["value"]
                        edit = {"operationId": op, "contextToken": edit_context["contextToken"],
                                "preconditions": {"expectedBridgeEpoch": bridge, "expectedQueueRevision": 4 + offset},
                                "change": change}
                        if op == "removeQueueEntry":
                            # The adapter fills in the token and preconditions for a caller-chosen ID.
                            edit = {"operationId": op, "change": change}
                        mcp.start("ytm_edit_queue", edit, 30 + offset)
                        cmd = daemon.receive(lambda frame: frame["kind"] == "player.command"
                                             and frame["payload"]["op"] == op)["payload"]["args"]
                        assert cmd["queueEntryId"] == change["entryId"]
                        assert cmd.get("beforeQueueEntryId") == change.get("beforeEntryId")
                        by_id = {item["entryId"]: item for item in items}
                        items[:] = [by_id[entry_id] for entry_id in expected_order]
                        send_queue(7 + offset, 5 + offset)
                        edited = mcp.result(30 + offset)["structuredContent"]["value"]
                        assert edited["phase"] == "verified", edited
                    # Retrying that ID returns its operation instead of a conflict from fresh state.
                    retried = mcp.call("ytm_edit_queue", edit, 40)["structuredContent"]["value"]
                    assert retried.get("operationId") == "removeQueueEntry" and retried["phase"] == "verified", retried
                    assert retried["revision"] == edited["revision"], retried

                    autoplay = {"operationId": "autoplay-on", "preconditions": {"expectedBridgeEpoch": bridge,
                                "expectedQueueRevision": 6}, "change": {"kind": "autoplay", "enabled": True}}
                    mcp.start("ytm_set_playback_mode", autoplay, 10)
                    cmd = daemon.receive(lambda frame: frame["kind"] == "player.command"
                                         and frame["payload"]["op"] == "setAutoplay")["payload"]
                    assert cmd["args"]["autoplay"] is True and cmd["args"]["queueRevision"] == 6
                    player["autoplay"] = True
                    daemon.send(host("bridge.playbackObserved", 9, player, core=epoch, bridge=bridge))
                    assert mcp.result(10)["structuredContent"]["value"]["phase"] == "verified"

                    # Without an operation ID or preconditions the adapter fills both in.
                    mcp.start("ytm_control_playback", {"action": "pause"}, 8)
                    daemon.receive(lambda frame: frame["kind"] == "player.command" and frame["payload"]["op"] == "pause")
                    daemon.send(host("bridge.playbackObserved", 10, {**player, "playback": "paused"}, core=epoch, bridge=bridge))
                    paused_result = mcp.result(8)
                    paused = paused_result["structuredContent"]["value"]
                    assert paused["phase"] == "verified" and paused["operationId"].startswith("auto-"), paused
                    assert paused_result["content"][0]["text"] == "Done.", paused_result["content"]
                    # A wait after the agent spoke stays bounded by its own short maxWaitSeconds.
                    started = time.monotonic()
                    waited = mcp.call("ytm_wait_dj_prompt", {"spoke": True, "maxWaitSeconds": 1}, 9)
                    assert time.monotonic() - started < 5
                    prompt = waited["structuredContent"]["value"]
                    assert prompt == {"cue": None, "chattiness": "normal"}, prompt
                    assert len(waited["content"][0]["text"]) < 80, waited["content"]
                    print("ordered enqueue, exact queue edits, observed codec and autoplay, replay and conflicting ID passed")

                    # The browser now has one played occurrence before the paused track.
                    items[0], items[1] = items[1], items[0]
                    send_queue(11, 7)
                    daemon.receive(lambda frame: frame["kind"] == "core.delta"
                                   and frame["payload"].get("deltaType") == "queueReplaced"
                                   and frame["payload"]["queue_summary"]["queueRevision"] == 7)
                    clear_commands = set()
                    for sequence, (operation_id, dispatched) in enumerate((
                        ("clear-refused", False), ("clear-upcoming", True),
                    ), start=12):
                        before = mcp.call("ytm_get_context", {}, 2 * sequence + 20)["structuredContent"]["value"]
                        clear = {"operationId": operation_id, "contextToken": before["contextToken"],
                                 "preconditions": {"expectedBridgeEpoch": bridge, "expectedQueueRevision": 7},
                                 "change": {"action": "clearUpcoming"}}
                        mcp.start("ytm_edit_queue", clear, 2 * sequence + 21)
                        command = daemon.receive(lambda frame: frame["kind"] == "player.command"
                                                 and frame["payload"]["op"] == "clearUpcoming")["payload"]
                        assert command["args"] == {"queueEntryId": "entry-now", "queueRevision": 7,
                                                   "trackInstanceId": "fixture-instance"}, command
                        assert command["commandId"] not in clear_commands, command
                        clear_commands.add(command["commandId"])
                        ack = {"commandId": command["commandId"], "bridgeEpoch": bridge, "dispatched": dispatched}
                        if not dispatched:
                            ack["error"] = {"code": "UNSUPPORTED_CAPABILITY", "retriable": False,
                                            "safeMessage": "Queue continuation is not finite"}
                        daemon.send(host("bridge.commandAck", sequence, ack, core=epoch, bridge=bridge))
                        if dispatched:
                            ledger_reader = LocalConnection(path)
                            assert ledger_reader.hello(token)["error"] is None
                            pending = ledger_reader.request("getOperation", {"operationId": operation_id})["value"]
                            ledger_reader.close()
                            assert not pending["schedulingStopped"], pending
                            items.pop()
                            send_queue(14, 8)
                            settled = mcp.result(2 * sequence + 21)["structuredContent"]["value"]
                            assert settled["phase"] == "verified" and settled["verification"]["level"] == "observed", settled
                        else:
                            refused = mcp.result(2 * sequence + 21)
                            settled = refused["structuredContent"]["value"]
                            assert settled["phase"] == "rejected" and settled["error"]["code"] == "CAPABILITY_UNAVAILABLE", settled
                            # The refusal names a way out in the same sentence.
                            text = refused["content"][0]["text"]
                            assert text.startswith("Not done") and "ytm_enqueue" in text and "next" in text, text
                    queue = mcp.call("ytm_get_queue", {"limit": 3}, 51)["structuredContent"]["value"]
                    player_after = mcp.call("ytm_get_context", {}, 52)["structuredContent"]["value"]["player"]
                    assert [item["entryId"] for item in queue["entries"]] == ["entry-C", "entry-now"], queue
                    assert queue["currentEntryId"] == "entry-now" and queue["revision"] == 8, queue
                    assert player_after["playback"] == "paused" and player_after["trackInstanceId"] == "fixture-instance", player_after
                    print("clear upcoming rejected an unsupported queue and preserved past and current occurrences on readback")

                    daemon.send(host("ui.playerCommand", 3, {"commandId": "manual-play", "op": "play"}, core=epoch))
                    own_events = list(own.until_event("manualTakeover"))
                    observer_events = list(observer.until_event("manualTakeover"))
                    assert any(event["event"] == "operationChanged" and event["operationId"] == "queue-three" for event in own_events)
                    assert not any(event["event"] == "operationChanged" for event in observer_events)
                    assert own_events[-1] == observer_events[-1]
                    daemon.receive(lambda frame: frame["kind"] == "core.delta" and frame["payload"].get("deltaType") == "manualTakeover")
                    own.close()
                    observer.close()
                    print("manual takeover reached the UI and both subscribers, operation events stayed with their principal")
                    daemon.send(host("ui.agentActivity", 4, {"action": "read"}, core=epoch, request="agent-fixture"))
                    activity = daemon.receive(lambda frame: frame.get("requestId") == "agent-fixture")
                    assert activity["kind"] == "core.agentActivity", activity
                    refill = next(row for row in activity["payload"]["operations"] if row["operationId"] == "queue-three")
                    assert refill["phase"] == "verified" and refill["verifiedItems"] == 3 and not refill["canCancel"]
                    print("desktop activity read the same verified SQLite receipt")

                    # A queue occurrence played without bookkeeping gets its token and queue revision.
                    mcp.start("ytm_play_context", {"target": {"kind": "queueEntry", "entryId": "entry-C"}}, 60)
                    command = daemon.receive(lambda frame: frame["kind"] == "player.command"
                                             and frame["payload"]["op"] == "selectQueueEntry")["payload"]
                    assert command["args"]["queueEntryId"] == "entry-C" and command["args"]["queueRevision"] == 8, command
                    daemon.send(host("bridge.commandAck", 15, {"commandId": command["commandId"], "bridgeEpoch": bridge,
                                                              "dispatched": True}, core=epoch, bridge=bridge))
                    # The selected occurrence counts only while its own video plays.
                    player = {**player, "playback": "playing", "currentQueueEntryId": "entry-C",
                              "trackInstanceId": "fixture-instance-c",
                              "currentTrack": {**player["currentTrack"], "videoId": "C", "title": "C"}}
                    daemon.send(host("bridge.playbackObserved", 16, player, core=epoch, bridge=bridge))
                    selected = mcp.result(60)["structuredContent"]["value"]
                    assert selected["phase"] == "verified", selected
                    playing_video = "C"
                    print("a queue occurrence played without bookkeeping was accepted and verified")

                    # Content-only harness check: discard structuredContent and use only content[0].text.
                    def text_value(result):
                        return json.loads(result["content"][0]["text"].split("\n", 1)[1])["value"]
                    ctx_data = text_value(mcp.call("ytm_get_context", {}, 53))
                    # ytm_search needs a signed-in catalog the synthetic daemon lacks, so the paged read is the queue:
                    # the second page is reached only through the nextCursor in the first page's text.
                    first_page = text_value(mcp.call("ytm_get_queue", {"limit": 1}, 54))
                    assert len(first_page["entries"]) == 1 and first_page["nextCursor"], first_page
                    second_page = text_value(mcp.call("ytm_get_queue", {
                        "limit": 1, "cursor": first_page["nextCursor"]}, 55))
                    assert second_page["entries"][0]["entryId"] != first_page["entries"][0]["entryId"], second_page
                    picked_video = second_page["entries"][0]["track"]["videoId"]
                    mcp.start("ytm_enqueue", {
                        "operationId": "content-only-enqueue",
                        "contextToken": ctx_data["contextToken"],
                        "mode": "next",
                        "videoIds": [picked_video],
                    }, 56)
                    frame = daemon.receive(lambda f: f["kind"] == "player.command" and f["payload"]["op"] == "enqueue")
                    assert frame["payload"]["args"]["target"]["videoId"] == picked_video
                    daemon.send(host("bridge.commandAck", 17, {
                        "commandId": frame["payload"]["commandId"],
                        "bridgeEpoch": bridge,
                        "dispatched": True,
                    }, core=epoch, bridge=bridge))
                    current_index = next(
                        idx for idx, item in enumerate(items) if item["entryId"] == "entry-now"
                    )
                    items.insert(current_index + 1, {"entryId": "entry-content-only", "videoId": picked_video,
                                     "track": {"videoId": picked_video, "title": picked_video, "availability": "available"},
                                     "availability": "available"})
                    send_queue(18, 9)
                    assert mcp.result(56)["structuredContent"]["value"]["phase"] == "verified"
                    print("content-only reads followed nextCursor and enqueued a videoId taken from the text alone")

                    lossy_runtime = root / "lossy-runtime"
                    lossy_runtime.mkdir(mode=0o700)
                    relay = LossyRelay(lossy_runtime, path)
                    lossy = init_mcp(mcp_bin, credential, {**env, "XDG_RUNTIME_DIR": str(lossy_runtime)})
                    try:
                        # A lost reply is looked up by the generated ID rather than reported blind.
                        relay.next_submit = "drop"
                        lossy.start("ytm_control_playback", {"action": "pause"}, 61)
                        daemon.receive(lambda frame: frame["kind"] == "player.command"
                                       and frame["payload"]["op"] == "pause")
                        player = {**player, "playback": "paused"}
                        daemon.send(host("bridge.playbackObserved", 19, player, core=epoch, bridge=bridge))
                        found = lossy.result(61)["structuredContent"]["value"]
                        assert found.get("operationId", "").startswith("auto-") and found["phase"] == "verified", found
                        # A submit that never arrived reports the generated ID it can be looked up by.
                        relay.next_submit = "swallow"
                        missing = lossy.call("ytm_control_playback", {"action": "resume"}, 62)
                        assert missing["isError"] and "auto-" in missing["structuredContent"]["value"]["message"], missing
                        assert missing["content"][0]["text"].startswith("Unclear"), missing["content"]
                        assert "operation" not in missing["content"][0]["text"], missing["content"]
                        # A stale refusal is returned to the agent, never resubmitted against the user.
                        relay.next_submit, before = "stale", relay.submits
                        refused = lossy.call("ytm_control_playback", {"action": "resume"}, 63)
                        assert refused["structuredContent"]["value"].get("code") == "STALE_CONTEXT", refused
                        assert "ytm_get_context" in refused["content"][0]["text"], refused["content"]
                        assert relay.submits == before + 1, relay.submits
                    finally:
                        lossy.close()
                        relay.close()
                    print("lost write replies were looked up by operationId and stale refusals were not resubmitted")
            finally:
                mcp.close()
        print("real MCP stdio initialization, tools and reconnect passed against one daemon")
    finally:
        daemon.close()
    assert not path.exists()
    print("socket removed on clean daemon shutdown")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("daemon", type=Path)
    parser.add_argument("mcp", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="ytm-mcp-offline-") as temp:
        run(args.daemon.resolve(), args.mcp.resolve(), Path(temp))
