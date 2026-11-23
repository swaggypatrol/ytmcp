# MCP server

`ytm-glass-mcp` is a Model Context Protocol server over stdio. An AI agent such as Claude Code starts it as a local MCP server, and it controls the running YTM Glass through the daemon's private control socket. Audio still comes from the official web player; the agent uses the same command path and sees the same state as the interface and MPRIS.

```
agent ── MCP stdio ──▶ ytm-glass-mcp ── one connection per call ──▶ $XDG_RUNTIME_DIR/ytm-glass/control.sock
                                                                    (0600, current user only)
```

## Registration

Each client is registered once from a terminal. Registration does not start the player.

```sh
ytm-glass-mcp register --principal NAME [--grant TIER_OR_ACTION]... [--playlist PLAYLIST_ID]... \
  [--account-scope SCOPE|signed-out] [--hours 1-168]
```

- **Default account.** Without `--account-scope`, the client is bound to the account currently signed in to the player, read from the player's database without changing it. If no account is signed in, registration fails and asks you to sign in first, or to pass `--account-scope signed-out` for playback-only control.
- **Stable credential path.** The command prints the path of the client's credential file, `$XDG_CONFIG_HOME/ytm-glass/control-credentials/NAME.json` (0600). Registering the same name again replaces the token but keeps the path, so the MCP client configuration does not change. The registration list stores only a SHA-256 of each token.
- **Lifetime.** Without `--hours`, a registration without edit-tier grants does not expire, and one with any edit-tier grant expires after 24 hours. `--hours` (1 to 168) sets the lifetime explicitly.
- **Limits.** At most 16 registrations are active at once.
- **First start.** The daemon opens the control socket at startup only if the registration list exists, so restart YTM Glass once after the first registration. Later registrations and removals take effect immediately: every handshake and request rereads the list. Removing a client's entry from `control-clients.json` revokes it.

Then add the server to your MCP client:

```sh
claude mcp add ytm -s user -- /usr/bin/ytm-glass-mcp \
  --credential-file ~/.config/ytm-glass/control-credentials/NAME.json --allow-background-start
```

For clients configured by JSON, use the same command and arguments:

```json
{
  "mcpServers": {
    "ytm": {
      "command": "/usr/bin/ytm-glass-mcp",
      "args": ["--credential-file", "/home/USER/.config/ytm-glass/control-credentials/NAME.json", "--allow-background-start"]
    }
  }
}
```

With `--allow-background-start`, a call made while the player is closed starts it with `--background --no-activate`: no window opens and nothing takes focus. The agent exiting never closes the player. In Claude Code you can allow every tool of this server at once by adding `mcp__ytm__*` to `permissions.allow` in your settings; the gates that matter are in the player.

## Grants and tiers

Grants follow what a mistake would cost, not whether a tool reads or writes.

| Tier | Grant | Covers | If it goes wrong | Limits |
| --- | --- | --- | --- | --- |
| Read | any registration | search; the queue, playlists, library, history, lyrics, settings | nothing changes | none |
| Play | `dj` (= `playback`, `queue`, `preferences`, and taste notes) | playback, queue, interruptions, queue edits, volume and modes, device settings, lyric offsets, DJ taste notes | a wrong song, fixed with one key press | none; manual priority and fences apply |
| Record | `record` (= `rating`, `library-membership`, `artist-subscription`, `create-private-playlist`) | likes and dislikes, library saves, artist subscriptions, new private playlists and adding to playlists the client created | entries that can be undone one by one | 100 items per account per rolling 24 hours |
| Edit | `edit` (= `append-playlist`, `edit-playlist`, `delete-playlist`, `local-history`) plus `--playlist` for each playlist | adding to existing playlists, removing or moving their entries, renaming, deleting playlists (no tool yet), removing local history | data can be lost | 20 items per account per rolling 24 hours, at most 10 per call; time-limited registration |

- Fine-grained action names (`playback`, `queue`, `rating`, …) can be granted one by one. `preferences` allows device settings and lyric offsets, and `account-handoff` allows asking the listener to sign in or switch accounts.
- Playlist editing grants require explicit `--playlist` IDs, so `--grant edit` needs at least one.
- A signed-out registration allows only playback, queue, preferences and account handoff.
- Changing a playlist's privacy is never granted: only the listener changes it, in the player or on the YouTube Music website. Asking for `playlist-privacy` fails at registration, and the tool action is refused.
- Limits count items: one like is 1, adding 30 songs is 30, removing 5 entries is 5. They are counted from the operation journal by when each operation was claimed, so they survive restarts. Refused and cancelled operations do not count. A refused call names the tier, the limit and when room frees up.
- With only `dj`, every account write is refused.

## How a call runs

1. The adapter opens a new connection for each tool call and authenticates with the client name and token.
2. The daemon checks the token, the expiry and that the registered account scope matches the player's current account; any mismatch is refused.
3. Reads need a valid registration; writes also need the matching grant.
4. The adapter waits up to 22 seconds for a reply; the daemon gives a read at most 20 seconds. A reply is at most 64 KiB, beyond which the tool asks for a smaller page. Pages hold at most 100 items, and a write takes at most 50 items.
5. The structured content of each result is the full JSON reply. The text content is one plain sentence, except for `ytm_wait_dj_prompt`, whose text carries the whole prompt so clients that show only text still get it.

## Writes

The agent states only what it wants, for example `ytm_enqueue {"videoIds": ["…"], "mode": "next"}`.

- **Bookkeeping is optional.** Write tools accept `operationId`, `preconditions` and `contextToken`, but they are optional. When they are left out the adapter generates an operation ID and fills in the context token and preconditions (account and page epochs, and the queue revision for queue writes) from the state at the time of the call.
- **Settled results.** After submitting, the adapter polls every 300 ms until the operation settles or 12 seconds pass. An interruption returns once the inserted tracks play and the resume is pending.
- **Four plain outcomes.** The text says one of: *Done*; *Partly done* (naming which items went through); *Not done; nothing changed* (with one way out, such as "Queue the songs with ytm_enqueue mode next instead"); or *Unclear* (check the state with `ytm_get_context` before trying again). The receipt phases (`accepted`, `submitted`, `acknowledged`, `verified`, `partial`, `rejected`, `unknown`, `cancelled`) stay in the structured content for debugging.
- **No automatic retry.** A write refused as stale is returned to the agent, which rereads and decides: the listener may have just taken over. If the reply to a write is lost, the adapter looks the operation up by its ID for up to 5 seconds instead of sending it again. A command that never reached the page is reported as not done, never as unclear.

Inside the player the usual guarantees stay in place:

1. **Journal before sending.** Each write takes a slot in the SQLite journal before it is sent. The same ID with the same request returns the original result; the same ID with a different request is a conflict. At most 16 writes run at once; beyond that the answer is `BUSY`.
2. **Submitted before sending.** A write is marked submitted right before dispatch, so a restart can tell "certainly not sent" from "possibly sent".
3. **Fenced playback.** Playback and queue commands carry a fence and are checked three times before reaching the page; manual input invalidates them. See [architecture.md](architecture.md#commands-and-fences).
4. **Evidence.** Playback and queue writes are confirmed by observation; ratings, subscriptions, library and playlist writes by a fresh read; settings, local history and taste notes by the local write.
5. **Removal ledger.** Before an edit-tier removal is sent, its receipt records what it takes away (for a playlist entry: the playlist, the entry, the song and its position; for local history: the entry, the song and when it was recorded), so it can be put back by hand.

Queue specifics:

- **Play next** inserts several tracks after the current one in input order, confirming each.
- **Add to the end** is available only when the end of the explicit queue is loaded; the tracks land before the autoplay suggestions.
- **Interrupt** plays tracks now with one of three resume policies: back to the interrupted track at its position, back to the original upcoming queue, or stay in the new context. Manual input abandons the resume.
- **Clear and replace upcoming** remove the loaded entries after the current one (the nearest 50 at most), never the current or earlier ones and never the autoplay suggestions. When more entries remain or may load later, as on real album queues, which carry a continuation marker, the result is partly done; replacing still inserts its tracks in order. Radio queues are refused. See [known-issues.md](known-issues.md).

### After a restart

Operations that had not settled are stopped, and a pending interruption resume is abandoned. Once the account is bound again, the player settles on its own the stopped operations whose outcome a fresh read can establish: new private playlists, appends, ratings, artist subscriptions and library saves (newest first, up to 20). Nothing is resent. Playback and queue writes have no evidence after the fact and stay unclear, as do playlist removals, moves and renames.

### Writes that still take explicit input

- Local history edits need the current history revision from `ytm_get_history`.
- Editing an existing playlist needs a playlist listed at registration.
- Adding songs to a playlist the client just created, under the record tier, takes `createdByOperationId` from the `ytm_create_playlist` result, which names it.
- Rating "this song" is best done with an explicit `videoId`.

## Tools

All 39 tools are published to every client; what a call may do still depends on its grants.

| Category | Tools |
| --- | --- |
| AI DJ (2) | `ytm_wait_dj_prompt`, `ytm_set_dj_feedback` |
| Playback and queue (8) | `ytm_control_playback`, `ytm_play_context`, `ytm_enqueue`, `ytm_interrupt_with`, `ytm_edit_queue`, `ytm_seek`, `ytm_set_volume`, `ytm_set_playback_mode` |
| Player state (8) | `ytm_get_context`, `ytm_get_player`, `ytm_get_queue`, `ytm_get_track`, `ytm_get_account`, `ytm_get_capabilities`, `ytm_get_settings`, `ytm_get_lyrics` |
| Discovery and catalog (10) | `ytm_search`, `ytm_get_recommendations`, `ytm_get_home`, `ytm_get_album`, `ytm_get_artist`, `ytm_get_playlist`, `ytm_get_library`, `ytm_get_history`, `ytm_get_extended_media`, `ytm_get_share_link` |
| Account and history writes (8) | `ytm_set_rating`, `ytm_set_library_membership`, `ytm_set_artist_subscription`, `ytm_create_playlist`, `ytm_add_playlist_entries`, `ytm_edit_playlist_entries`, `ytm_update_playlist`, `ytm_manage_history` |
| Settings and account (3) | `ytm_update_settings`, `ytm_set_lyrics_offset`, `ytm_account_handoff` |

Notes on a few tools:

- `ytm_get_context` captures the current song, playback state and queue together; its context token is what the adapter fills into later writes.
- `ytm_search` searches one entity kind (song, album, artist or playlist); its suggestions mode returns anonymous public predictions.
- `ytm_get_recommendations` finds song radio candidates without changing playback.
- `ytm_get_history` reads local history (observed playback instances with a revision), YouTube history, or both side by side.
- `ytm_account_handoff` asks the listener to sign in, renew or switch accounts in the player. Nothing opens or takes focus; the request waits in the activity view until the change is observed or 30 minutes pass.
- `ytm_get_settings` with section `djPreferences` pages the recorded taste notes, newest first.

The operation journal tools (`ytm_get_operation`, `ytm_cancel_operation`, `ytm_resume_operation`) exist in the control protocol but are not published to agents. The old `--profile` argument is accepted and ignored.

## Offline check

```sh
just test-control
```

This builds the daemon and adapter and drives MCP negotiation, grants, ordered inserts, operation replay and conflicts, playback confirmation and shutdown against one synthetic daemon (`tools/mcp/offline-smoke.py`). Checks against the real page are described in [building.md](building.md#live-checks).
