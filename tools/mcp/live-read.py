#!/usr/bin/env python3
"""Check a packaged YTM Glass against its own existing player profile, using read-only MCP grants."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import tempfile
import time


spec = importlib.util.spec_from_file_location("offline_smoke", Path(__file__).with_name("offline-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
smoke.TIMEOUT = 22

PROBES = {
    "account": ("ytm_get_account", {}),
    "home": ("ytm_get_home", {"kind": "home", "limit": 8}),
    "explore": ("ytm_get_home", {"kind": "explore", "limit": 8}),
    "moods": ("ytm_get_home", {"kind": "moodsAndGenres", "limit": 8}),
    "moodCategory": ("ytm_get_home", {"kind": "moodsAndGenres", "limit": 8}),
    "charts": ("ytm_get_home", {"kind": "charts", "limit": 8}),
    "newReleases": ("ytm_get_home", {"kind": "newReleases", "limit": 8}),
    "playlistLibrary": ("ytm_get_library", {"kind": "playlists", "page": {"limit": 8}}),
    "uploads": ("ytm_get_library", {"kind": "uploads", "page": {"limit": 8}}),
    "podcasts": ("ytm_get_library", {"kind": "podcasts", "page": {"limit": 8}}),
    "search": ("ytm_search", {"query": "instrumental piano", "limit": 8}),
    "albumSearch": ("ytm_search", {"query": "jazz", "kind": "album", "limit": 8}),
    "albumMembership": ("ytm_get_album", {"page": {"limit": 3}, "includeMembership": True}),
    "artistSearch": ("ytm_search", {"query": "jazz", "kind": "artist", "limit": 8}),
    "playlistSearch": ("ytm_search", {"query": "jazz", "kind": "playlist", "limit": 8}),
    "searchSuggestions": ("ytm_search", {"query": "instrumental pian", "mode": "suggestions", "limit": 8}),
    "recommendations": ("ytm_get_recommendations", {"page": {"limit": 8}}),
    "savedSongs": ("ytm_get_library", {"kind": "songs", "page": {"limit": 8}}),
    "savedAlbums": ("ytm_get_library", {"kind": "albums", "page": {"limit": 8}}),
    "libraryArtists": ("ytm_get_library", {"kind": "artists", "page": {"limit": 8}}),
    "subscriptions": ("ytm_get_library", {"kind": "subscriptions", "page": {"limit": 8}}),
    "youtubeHistory": ("ytm_get_history", {"source": "youtube", "page": {"limit": 8}}),
    "localHistory": ("ytm_get_history", {"source": "local", "page": {"limit": 8}}),
    "combinedHistory": ("ytm_get_history", {"source": "combined", "page": {"limit": 8}}),
}
EXPECTED_PAGES = {
    "moodCategory": ("discovery", "youtube_music_mood_playlists"),
    "charts": ("discovery", "youtube_music_charts"),
    "newReleases": ("discovery", "youtube_music_new_releases"),
    "searchSuggestions": ("searchSuggestions", "youtube_music_search_suggestions"),
    "localHistory": ("localHistory", "local_playback_history"),
    "albumSearch": ("searchEntities", "youtube_music_search_albums"),
    "artistSearch": ("searchEntities", "youtube_music_search_artists"),
    "playlistSearch": ("searchEntities", "youtube_music_search_playlists"),
}


def report(record):
    print(json.dumps(record, ensure_ascii=False), flush=True)


def register(mcp, env, principal, scope, *, playback=False):
    argv = [mcp, "register", "--principal", principal, "--account-scope", scope, "--hours", "1"]
    if playback:
        argv.extend(["--grant", "playback"])
    process = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=5, check=True)
    return Path(process.stdout.strip().splitlines()[-1])


def wait_for_session(app, root, socket_path):
    database = root / "data/ytm-glass/ytm-glass.db"
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and app.poll() is None:
        if database.is_file() and socket_path.is_socket():
            try:
                with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
                    account = db.execute("SELECT account_scope FROM account_epochs WHERE state = 'active' "
                                         "ORDER BY updated_at_ms DESC LIMIT 1").fetchone()
                    if account:
                        return account[0]
            except sqlite3.OperationalError:
                pass
        time.sleep(0.2)
    return None


def result_page(content):
    value = content["value"]
    if content["result"] == "discovery":
        return value["items"]
    if content["result"] == "recommendations":
        return value["candidates"]
    if content["result"] == "library":
        return value["page"].get("entries", value["page"])
    if content["result"] == "localHistory":
        return value["entries"]
    return value


def matches_page(page, source):
    return page["source"] == source and page["freshness"] == "fresh" and len(page["items"]) <= 8


def search_entity_ids(page, kind):
    items = page["items"]
    key = f"{kind}Id"
    if any(row["kind"] != kind or not row.get(key) for row in items):
        return None
    ids = {row[key] for row in items}
    return ids if len(ids) == len(items) else None


def operation_reader(credential):
    """Returns a function that reads an operation's receipt over the player's local control
    socket. Agents no longer get the ledger tools; live checks still follow long operations."""
    data = json.loads(Path(credential).read_text())
    path = Path(os.environ["XDG_RUNTIME_DIR"]) / "ytm-glass/control.sock"

    def read(operation_id):
        connection = smoke.LocalConnection(path)
        try:
            hello = connection.hello(data["token"], data["principal"])
            if hello.get("error"):
                return hello["error"], True
            response = connection.request("getOperation", {"operationId": operation_id})
            return response["value"], response["result"] == "error"
        finally:
            connection.socket.close()

    return read


def public_album_id(client, identifier):
    search = client.call("ytm_search", {"query": "Kind of Blue Miles Davis", "kind": "album", "limit": 8}, identifier)
    if search["isError"] or search["structuredContent"]["result"] != "searchEntities":
        return None
    return next((item["albumId"] for item in result_page(search["structuredContent"])["items"]
                 if item["kind"] == "album" and item["title"].strip().lower() == "kind of blue"), None)


def check_combined_history(client, stage, tool, arguments, content, identifier, follow_cursors):
    if content["result"] != "combinedHistory":
        return dict(stage=stage, status="FAIL", reason="unexpected response type")
    value = content["value"]
    local = value["local"]["entries"]
    youtube = value["youtube"]
    if value["youtubeError"] or youtube is None:
        return dict(stage=stage, status="FAIL", count=len(local["items"]),
                    code=(value["youtubeError"] or {}).get("code"))
    valid = bool(value["local"]["revision"]) and matches_page(local, "local_playback_history") and (
        matches_page(youtube, "youtube_music_recent_history")
    )
    record = dict(stage=stage, status="PASS_TARGET" if valid else "FAIL",
                  count=len(local["items"]), youtubeCount=len(youtube["items"]),
                  nextCursor=bool(local["nextCursor"] or youtube["nextCursor"]))
    if follow_cursors and record["nextCursor"]:
        following = {**arguments, "localCursor": local["nextCursor"], "youtubeCursor": youtube["nextCursor"]}
        reply = client.call(tool, following, identifier + 1)
        next_content = reply["structuredContent"]
        if reply["isError"]:
            record["secondPage"] = dict(status="FAIL", code=next_content["value"].get("code"))
            return record
        next_record = check_combined_history(client, stage, tool, arguments, next_content, identifier + 1, False)
        if next_record["status"] == "PASS_TARGET":
            next_value = next_content["value"]
            for previous, current in ((local, next_value["local"]["entries"]), (youtube, next_value["youtube"])):
                if previous["nextCursor"] and current["items"] == previous["items"]:
                    next_record["status"] = "FAIL"
        record["secondPage"] = {key: next_record[key] for key in ("status", "count", "youtubeCount", "code")
                                if key in next_record}
    return record


def playback_witness(client, identifier):
    result = client.call("ytm_get_context", {"previewLimit": 0}, identifier)
    context = result["structuredContent"]["value"]
    player = context["player"]
    return {"bridgeEpoch": context["bridgeEpoch"], "queueRevision": context["queue"]["revision"],
            **{key: player[key] for key in ("connection", "trackInstanceId", "playback", "shuffle", "repeat", "autoplay")}}


def stable_playback(client):
    previous = None
    for identifier in range(10, 36):
        current = playback_witness(client, identifier)
        if current == previous and current["connection"] == "ready" and current["queueRevision"] > 0:
            return current
        previous = current
        time.sleep(0.2)
    return None


def examine(mcp, credential, env, stage, tool, arguments, follow_cursors):
    client = smoke.init_mcp(mcp, credential, env)
    try:
        identifier = 2
        if stage == "moodCategory":
            landing = client.call(*PROBES["moods"], identifier)
            if landing["isError"] or landing["structuredContent"]["result"] != "discovery":
                return dict(stage=stage, status="BLOCKED", reason="no verified mood landing page")
            index = result_page(landing["structuredContent"])
            if not matches_page(index, "youtube_music_moods_and_genres"):
                return dict(stage=stage, status="BLOCKED", reason="no verified mood landing page")
            category_id = next((item["target"]["categoryId"] for item in index["items"]
                                if item["target"]["kind"] == "moodCategory"), None)
            if category_id is None:
                return dict(stage=stage, status="BLOCKED", reason="no mood category on the landing page")
            arguments = {**arguments, "categoryId": category_id}
            identifier += 1
        if stage == "albumMembership":
            album_id = public_album_id(client, identifier)
            if album_id is None:
                return dict(stage=stage, status="BLOCKED", reason="the public album search did not give a matching release")
            arguments = {**arguments, "albumId": album_id}
            identifier += 1
        if stage == "recommendations":
            seed = client.call("ytm_search", PROBES["search"][1], identifier)
            if seed["isError"] or not seed["structuredContent"]["value"]["items"]:
                return dict(stage=stage, status="BLOCKED", reason="no verified search result to seed a recommendation")
            track = seed["structuredContent"]["value"]["items"][0]
            arguments = {**arguments, "seed": {"kind": "video", "videoId": track["videoId"]}}
            before = stable_playback(client)
            if before is None:
                return dict(stage=stage, status="BLOCKED", reason="no stable observed playback baseline")
            identifier = 40
        reply = client.call(tool, arguments, identifier)
        content = reply["structuredContent"]
        value = content["value"]
        if reply["isError"]:
            return dict(stage=stage, status="FAIL", code=value.get("code"))
        if content["result"] == "account":
            matched = value["state"] == "authenticated" and value["playerCatalogMatched"]
            return dict(stage=stage, status="PASS_TARGET" if matched else "FAIL",
                        state=value["state"], matched=value["playerCatalogMatched"])
        if stage == "combinedHistory":
            return check_combined_history(client, stage, tool, arguments, content, identifier, follow_cursors)
        if stage == "albumMembership":
            evidence = value.get("membership") or {}
            matched = content["result"] == "album" and value["album"]["id"] == arguments["albumId"]
            observed = isinstance(evidence.get("value"), bool) and evidence.get("observedAtMs", 0) > 0
            return dict(stage=stage, status="PASS_TARGET" if matched and observed else "FAIL",
                        releaseMatched=matched, membershipObserved=observed)
        expected = EXPECTED_PAGES.get(stage)
        if expected and content["result"] != expected[0]:
            return dict(stage=stage, status="FAIL", reason="unexpected response type")
        page = result_page(content)
        cursor = page.get("nextCursor")
        passed = matches_page(page, expected[1]) if expected else page.get("freshness") == "fresh"
        typed_kind = arguments.get("kind") if content["result"] == "searchEntities" else None
        first_ids = search_entity_ids(page, typed_kind) if typed_kind else None
        if typed_kind:
            passed = passed and bool(first_ids)
        if stage == "searchSuggestions":
            passed = passed and cursor is None and all(isinstance(item, str) and item for item in page["items"])
        elif stage == "moodCategory":
            passed = passed and all(item["target"]["kind"] == "playlist" for item in page["items"])
        observation = dict(stage=stage, status="PASS_TARGET" if passed else "FAIL",
                           count=len(page["items"]), nextCursor=cursor is not None)
        if expected is None:
            observation.update(source=page.get("source"), freshness=page.get("freshness"))
        if follow_cursors and cursor:
            if tool == "ytm_search":
                following = {**arguments, "cursor": cursor}
            else:
                following = {**arguments, "page": {"limit": 8, "cursor": cursor}}
            next_reply = client.call(tool, following, identifier + 1)
            next_content = next_reply["structuredContent"]
            if next_reply["isError"]:
                observation["secondPage"] = {"status": "FAIL", "code": next_content["value"].get("code")}
            else:
                next_page = result_page(next_content)
                scope_valid = True
                if typed_kind:
                    next_ids = search_entity_ids(next_page, typed_kind)
                    scope_valid = first_ids is not None and next_ids is not None and first_ids.isdisjoint(next_ids)
                observation["secondPage"] = {
                    "status": "PASS_TARGET" if scope_valid and next_page["source"] == page["source"] and next_page.get("freshness") == "fresh" and (
                        not next_page["items"] or next_page["items"] != page["items"]
                    ) else "FAIL",
                    "count": len(next_page["items"]),
                }
        if stage == "recommendations":
            after = playback_witness(client, identifier + 2)
            observation["changedPlaybackFields"] = [field for field in before if before[field] != after[field]]
            observation["playerUnchanged"] = not observation["changedPlaybackFields"]
            if not observation["playerUnchanged"]:
                observation["status"] = "FAIL"
        return observation
    finally:
        client.close()


def run_native_session(package, witness, run):
    electron = package / "opt/ytm-glass/ytm-glass"
    resources = electron.parent / "resources"
    mcp = package / "usr/bin/ytm-glass-mcp"
    runtime = Path(os.environ["XDG_RUNTIME_DIR"])
    socket_path = runtime / "ytm-glass/control.sock"
    running = any(subprocess.run(["pgrep", "-x", name], stdout=subprocess.DEVNULL).returncode == 0
                  for name in ("ytm-glass", "ytm-daemon"))
    if running or socket_path.exists():
        report(dict(stage="launch", status="BLOCKED", reason="an existing player owns the profile or control socket"))
        return 2
    with tempfile.TemporaryDirectory(prefix="isolated-store-", dir=witness) as isolated:
        root = Path(isolated)
        env = dict(os.environ, YTM_GLASS_DAEMON_BIN=str(resources / "bin/ytm-daemon"),
                   YTM_GLASS_UI_DIST_DIR=str(resources / "ui-dist"),
                   YTM_GLASS_BRIDGE_DIST_DIR=str(resources / "bridge-dist"),
                   YTM_GLASS_IDENTITY_PATH=str(resources / "config/app-identity.json"),
                   YTM_GLASS_DATA_DIR=str(root), RUST_LOG="warn")
        # The daemon only enables the listener when the local user has registered a client.
        register(mcp, env, "bootstrap-read-check", "signed-out")
        with (witness / "native-runtime.log").open("w") as log:
            app = subprocess.Popen([electron, "--background", "--no-activate"], env=env,
                                   stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
            try:
                scope = wait_for_session(app, root, socket_path)
                if not scope:
                    report(dict(stage="session", status="BLOCKED", socketReady=socket_path.is_socket(),
                                appExit=app.poll(), reason="no existing authenticated player session became available"))
                    return 2
                report(dict(stage="session", status="PASS_TARGET", runtime=os.environ.get("XDG_SESSION_TYPE")))
                return run(mcp, env, scope)
            finally:
                if app.poll() is None:
                    app.terminate()
                    try:
                        app.wait(timeout=6)
                    except subprocess.TimeoutExpired:
                        os.killpg(app.pid, signal.SIGKILL)
                        app.wait(timeout=4)


def check(package, witness, selected, follow_cursors):
    def read_catalog(mcp, env, scope):
        credential = register(mcp, env, "candidate-read-check", scope)
        failures = 0
        with ThreadPoolExecutor(max_workers=3) as pool:
            probes = [(stage, pool.submit(examine, mcp, credential, env, stage, tool, arguments, follow_cursors))
                      for stage, (tool, arguments) in PROBES.items() if not selected or stage in selected]
            for stage, probe in probes:
                try:
                    record = probe.result()
                except Exception as error:
                    record = dict(stage=stage, status="FAIL", error=type(error).__name__)
                failures += record["status"] != "PASS_TARGET" or record.get("secondPage", {}).get("status") == "FAIL"
                report(record)
        return int(failures > 0)

    return run_native_session(package, witness, read_catalog)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-root", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--only", choices=sorted(PROBES), action="append")
    parser.add_argument("--follow-cursors", action="store_true")
    args = parser.parse_args()
    os.umask(0o077)
    raise SystemExit(check(args.package_root.resolve(), args.evidence_dir.resolve(), args.only, args.follow_cursors))
