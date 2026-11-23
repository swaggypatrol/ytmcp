# Known issues

These are the known limits of the current code. Each describes what happens and, where there is one, the workaround.

## Playback and queue

| # | Issue |
| --- | --- |
| K2 | Before the official page has loaded any track, a play request returns the retryable error `UNAVAILABLE_TRACK`. Once a track is loaded, playing works. |
| K12 | After switching from another context to an album, the page sometimes loads the whole album and plays the requested track while its internal selected entry stays on another row. Playback of the album is confirmed anyway, but a queue edit or interruption right after it can fail (seen once in three tries), because queue editing is disabled while the selection is inconsistent. Whether the page realigns after the next track change is not verified. |
| K13 | The page keeps its play queue in the player profile and restores it at the next start. Starting the same album's audio playlist then only jumps within the restored queue instead of reloading it. |
| K14 | "Clear upcoming" and "replace upcoming" are best effort: they remove the loaded entries after the current one, the nearest 50 at most, and say "partly done" when more may follow. Real album queues always carry a continuation marker, so more entries may load later and the result is "partly done" even when nothing followed; replacing still inserts its tracks. Both are refused on radio queues and while autoplay suggestions play. Whether removing entries makes the real page fetch its continuation is not verified. |
| K16 | While the page has an autoplay tail, the queue panel's remove, move and clear buttons stay disabled. Turn autoplay off in the queue panel to edit. |

## Catalog

| # | Issue |
| --- | --- |
| K17 | Song radio and playlist continuations refetch upstream pages by offset without a window fingerprint, so if the upstream reorders, paging can repeat or skip items. In typed search, one row that cannot be parsed fails the whole page. Each search suggestion request creates a new anonymous client. A playlist continuation reads the first page again for metadata. |

## Desktop

| # | Issue |
| --- | --- |
| K3 | Under KWin on Wayland, a second launch is handled by the running instance and its window. The window can stay minimized with KWin's "demands attention" flag set; it can be found through the taskbar's attention hint. |
| K7 | The GPU and network broker helper processes connect to Xwayland. The main window itself is native Wayland, with GPU compositing and rasterization enabled. |
| K15 | With track-change notifications on, the KDE notification never takes focus but can appear above a fullscreen game. Notifications are off by default. |

## MCP and AI DJ

| Issue |
| --- |
| Many MCP clients, Claude Code included, hold a message the user sends during a tool call until the call returns. Conversation-hot waits cap this at about a minute for five minutes after either side speaks; otherwise a wait can last until the next prompt. A shorter `maxWaitSeconds` or interrupting the call (Esc in Claude Code) also helps. |
| Actions taken directly on the hidden official page are not seen as listener interaction, so they neither reset the check-in heartbeat nor count as the listener's skip. |
| `ytm_delete_playlist` and an audio quality tool are not implemented, so the edit tier cannot delete playlists yet. The `delete-playlist` grant exists for when it can. |
| Playback and queue writes interrupted by a restart cannot be confirmed afterwards and stay unclear. |

## Untested

- Platforms other than an Arch-based distribution with KDE Plasma 6 on native Wayland: other desktops, X11 sessions, other GPUs.
- The liquid glass material has been measured on one high-end GPU only; its cost on weaker GPUs is unknown. Reduced transparency in Settings turns it off.
- How the glass looks on an HDR display has not been judged by eye; color measurements come from captured frames.
- Whether KDE's "reduce animations" and high-contrast settings reach Chromium as `prefers-reduced-motion` and `forced-colors` is not verified; the app's own settings work.
