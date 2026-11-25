# Known issues

These are the known limits of the current code. Each describes what happens and, where there is one, the workaround.

## Playback and queue

| # | Issue |
| --- | --- |
| K2 | Right after the player page is ready, before it has reported what it has loaded, and while the last track is being restored, a play (from a media key, the tray or an agent) waits instead of failing: the restored track then plays from its saved position, or the page's own restored queue plays. It waits up to 10 seconds for the page and the saved session; once the restore has started, the restore's own time limits apply. Only when the page has nothing loaded and there is nothing to restore, or the wait ends, does it fail with the retryable error `UNAVAILABLE_TRACK`. A play sent before the player page is ready at all is still refused. The app's own Play button stays disabled until a track is loaded. Not yet confirmed against the live page. |
| K12 | After switching from another context to an album, the page sometimes loads the whole album and plays the requested track while its internal selected entry stays on another row. Playback of the album is confirmed anyway, but a queue edit or interruption right after it can fail (seen once in three tries), because queue editing is disabled while the selection is inconsistent. Whether the page realigns after the next track change is not verified. |
| K13 | The page keeps its play queue in the player profile and restores it at the next start. Starting the same album's audio playlist then only jumps within the restored queue instead of reloading it. |
| K14 | "Clear upcoming" and "replace upcoming" are best effort: they remove the loaded entries after the current one, the nearest 50 at most, and say "partly done" when more may follow. Real album queues always carry a continuation marker, so more entries may load later and the result is "partly done" even when nothing followed; replacing still inserts its tracks. Both are refused on radio queues and while autoplay suggestions play. Whether removing entries makes the real page fetch its continuation is not verified. |
| K16 | While the page has an autoplay tail, the queue offers no remove or move controls for single tracks; Clear upcoming still works. Turn autoplay off in the queue to edit single tracks. |

## Catalog

| # | Issue |
| --- | --- |
| K17 | Song radio and playlist continuations refetch upstream pages by offset without a window fingerprint, so if the upstream reorders, paging can repeat or skip items. In typed search, one row that cannot be parsed fails the whole page. Each search suggestion request creates a new anonymous client. A playlist continuation reads the first page again for metadata. |
| K20 | Adding a song to a playlist happens at the pick, with no confirmation step. When YouTube Music names the entry the addition created, the "Added to" toast offers Undo for as long as the toast is up, and for at most 5 minutes: it removes only that entry, never another copy of the song, and checks the playlist again before saying it is removed. When YouTube Music does not name the entry, the toast says the song was added and has no Undo; remove it on the playlist page or in YouTube Music. Undo has not been tried on the real page or a real account yet, not even on a test playlist. |
| K21 | Home shows recent plays and playlists. Personalized recommendations are planned. |

## Desktop

| # | Issue |
| --- | --- |
| K18 | Recent searches on the Search page are kept by the core for the signed-in account, so they should survive a restart; they are deleted on sign-out and kept by Clear cache. That they survive a restart is not yet confirmed in the installed app. A list that arrives after the blocks below it are already on screen is shown the next time the start of the page is shown, so nothing moves. |
| K19 | The Liked songs page has Play and Shuffle in its header when the app has just read the page's own playback positions for the signed-in account and the player is ready; otherwise they are not shown, and a liked song starts from its row. Play starts the list from its first song in order; Shuffle starts it there and then turns on YouTube Music's shuffle once the song has started. Whether YouTube Music's Liked rows carry those positions, and whether the page then plays, and with Shuffle shuffles, the whole list, has not been checked against the real page or a real account. |
| K3 | Under KWin on Wayland, a second launch is handled by the running instance and its window. The window can stay minimized with KWin's "demands attention" flag set; it can be found through the taskbar's attention hint. |
| K7 | The GPU and network broker helper processes connect to Xwayland. The main window itself is native Wayland, with GPU compositing and rasterization enabled. |
| K15 | With track-change notifications on, the KDE notification never takes focus but can appear above a fullscreen game. Notifications are off by default. |

## MCP and AI DJ

| Issue |
| --- |
| Many MCP clients, Claude Code included, hold a message the user sends during a tool call until the call returns. Conversation-hot waits cap this at about a minute for five minutes after either side speaks; otherwise a wait can last until the next prompt. A shorter `maxWaitSeconds` or interrupting the call (Esc in Claude Code) also helps. |
| Actions taken directly on the hidden official page are not seen as listener interaction, so they neither reset the check-in heartbeat nor count as the listener's skip. |
| `ytm_delete_playlist` and an audio quality tool are not implemented, so the edit tier cannot delete playlists yet. The `delete-playlist` grant exists for when it can. |
| Moment DJ never fetches weather (`weatherProvided` is always `false`). Assistants that need weather can look up the configured `sceneCity` using their own tools when allowed by the user. |
| Moment DJ: the listener can clear listening intents in the app but not type one in; intents come from assistants. Each `session` intent runs its own 6 hours rather than sharing one listening session. |
| Moment DJ: a listener request reported by an assistant (`basis: userRequest`) may still omit `expectedMomentRevision`; only inferences must carry it. |
| Moment DJ: holidays come from `date-holidays` as published, with English names; there is no per-holiday choice, and a substitute day off carries no `actualDate`. A change of the machine's time zone can take up to a minute to show. |
| Moment DJ shares are kept by assistant name. Registering a new assistant under a name that was shared before inherits that share; an assistant shared and then removed from the registrations stays listed in Settings so it can be switched off. |
| The queue page shows a 100-entry window around the current track; there is no paging beyond it in the app (agents can page the loaded window with `ytm_get_queue`). |
| YouTube Music's suggestions after the queue are read-only rows: they cannot be selected, moved or removed one by one. Starting a radio from the current song without interrupting it is not supported. |
| An Up Next filter id names the filter for the suggestions it was read with; within the same suggestions (for example the next song of the same radio) an old id still works. |
| `tools/mcp/dj_harness.py` is an adapter for hosts that run their own event loop; no agent harness on this machine feeds it listener speech yet. |
| Confirmed-unsent `Accepted` operations settle on restart to `Cancelled` (`NewPlanRequired`); playback and queue writes that may already have been dispatched cannot be confirmed afterwards and stay unclear (`Unknown`). |
| After an upgrade, restart MCP clients that were already running (for example an assistant session that started before the upgrade): an older `ytmcp` adapter cannot read fields the new player adds, such as `preparing` right after launch, and its reads fail in that moment. |
| `ytm_play_context` with a playlist and a start song uses the position YouTube Music gives each row on its own play button, so a song after a greyed-out row starts at the right place. Whether real playlist rows carry that position has not been checked; where a row does not, the row's position in the list is used, and after a greyed-out row the play is then reported as not confirmed instead of starting at the wrong song. |

## Untested

- The changes listed under Unreleased in the changelog were checked with unit tests and in the app's mock mode on one machine. Playing right after launch, covers on a song change, the volume wheel with a real player, recent searches after a restart, adding to one playlist while another is open, Play and Shuffle for Liked songs, and Undo after adding to a playlist have not been tried with the installed app and a real account yet.
- Reading YouTube Music's suggestions and filters, and choosing a filter through the app, on the live page with this release's bridge. The page's own behaviour (choosing a filter keeps the song playing and the queued songs) was observed live; this bridge's reading and clicking were checked only against synthetic pages.
- Platforms other than an Arch-based distribution with KDE Plasma 6 on native Wayland: other desktops, X11 sessions, other GPUs.
- The liquid glass material has been measured on one high-end GPU only; its cost on weaker GPUs is unknown. Reduced transparency in Settings turns it off.
- How the glass looks on an HDR display has not been judged by eye; color measurements come from captured frames.
- Whether KDE's "reduce animations" and high-contrast settings reach Chromium as `prefers-reduced-motion` and `forced-colors` is not verified; the app's own settings work.
