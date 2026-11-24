# Known issues

These are the known limits of the current code. Each describes what happens and, where there is one, the workaround.

## Playback and queue

| # | Issue |
| --- | --- |
| K2 | Before the official page has loaded any track, a play request returns the retryable error `UNAVAILABLE_TRACK`. Once a track is loaded, playing works. |
| K12 | After switching from another context to an album, the page sometimes loads the whole album and plays the requested track while its internal selected entry stays on another row. Playback of the album is confirmed anyway, but a queue edit or interruption right after it can fail (seen once in three tries), because queue editing is disabled while the selection is inconsistent. Whether the page realigns after the next track change is not verified. |
| K13 | The page keeps its play queue in the player profile and restores it at the next start. Starting the same album's audio playlist then only jumps within the restored queue instead of reloading it. |
| K14 | "Clear upcoming" and "replace upcoming" are best effort: they remove the loaded entries after the current one, the nearest 50 at most, and say "partly done" when more may follow. Real album queues always carry a continuation marker, so more entries may load later and the result is "partly done" even when nothing followed; replacing still inserts its tracks. Both are refused on radio queues and while autoplay suggestions play. Whether removing entries makes the real page fetch its continuation is not verified. |
| K16 | While the page has an autoplay tail, the queue offers no remove or move controls for single tracks; Clear upcoming still works. Turn autoplay off in the queue to edit single tracks. |

## Catalog

| # | Issue |
| --- | --- |
| K17 | Song radio and playlist continuations refetch upstream pages by offset without a window fingerprint, so if the upstream reorders, paging can repeat or skip items. In typed search, one row that cannot be parsed fails the whole page. Each search suggestion request creates a new anonymous client. A playlist continuation reads the first page again for metadata. |
| K20 | Adding a song to a playlist asks for confirmation. The desktop UI cannot undo that addition. |
| K21 | Home shows recent plays and playlists. Personalized recommendations are planned. |

## Desktop

| # | Issue |
| --- | --- |
| K18 | Recent searches on the Search page are kept for one run of the app and are empty after a restart. |
| K19 | The Liked songs page has no Play or Shuffle button in its header; start a liked song from its row. |
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

## Untested

- Reading YouTube Music's suggestions and filters, and choosing a filter through the app, on the live page with this release's bridge. The page's own behaviour (choosing a filter keeps the song playing and the queued songs) was observed live; this bridge's reading and clicking were checked only against synthetic pages.
- Platforms other than an Arch-based distribution with KDE Plasma 6 on native Wayland: other desktops, X11 sessions, other GPUs.
- The liquid glass material has been measured on one high-end GPU only; its cost on weaker GPUs is unknown. Reduced transparency in Settings turns it off.
- How the glass looks on an HDR display has not been judged by eye; color measurements come from captured frames.
- Whether KDE's "reduce animations" and high-contrast settings reach Chromium as `prefers-reduced-motion` and `forced-colors` is not verified; the app's own settings work.
