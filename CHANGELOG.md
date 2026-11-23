# Changelog

Versions are `<app version>-<package revision>`; see [docs/building.md](docs/building.md#versioning).

## 1.1.0-1

- **Renamed to YTMcp.** The app, the package (`ytmcp`, which replaces `ytm-glass`), the MCP server and registration command (`ytmcp`, formerly `ytm-glass-mcp`), the launcher (`ytmcp-player`), the desktop entry (`local.ytmcp.Desktop`), the MPRIS name (`org.mpris.MediaPlayer2.ytmcp`), the `YTMCP_*` environment variables and the XDG directories (`ytmcp`). On first start the app moves an existing install's config, data (including the browser profile, so you stay signed in) and cache directories to the new name, merging into anything already created under it without overwriting, and renames the database with them. If the old app is still running it asks you to quit it first; mock mode and an explicit `--user-data-dir` never migrate. When installing over `ytm-glass`, answer `y` when pacman asks to remove it. MCP clients registered before need their command changed to `/usr/bin/ytmcp` and their credential path to `~/.config/ytmcp/control-credentials/`. If you re-add the server to Claude Code under the new name `ytmcp`, its tools become `mcp__ytmcp__*`, so update `permissions.allow` too. Entries below this one use the old name.
- A new palette for both themes from eight Pantone pastels, one job per colour: Cloud Dancer paper by day and a plum-tinted night, Raindrops on Roses for what's playing, Orchid Tint for the AI DJ, Ice Melt for focus and information, Almost Aqua for done, Lemon Icing for attention and Peach Dust for failures. Cover colours keep tinting Now playing and Lyrics, at the same pastel strength. Settings switches use neutral ink.
- Liquid glass rebuilt on every surface (sidebar, player bar, dialogs) as one even frost that also refracts: the backdrop is blurred, then bent along the outline's real normal, so lines curve around the rounded corners, and content passing under an edge reappears upside down and compressed in the rim, as at the edge of thick glass. There are no clearer or cloudier zones, and nothing above the player bar is blurred before it reaches the glass. A thin specular line follows the edges facing the light. The bending map is nine small cached pieces; frame times match the opaque baseline and every screenshot contrast check passes.
- The app icon takes the same pastels.
- Seek and volume are flat rounded bars with no thumb: white for what has played, light grey for what has loaded (new `bufferedMs` in the playback observation and player state, from the official player's per-track loaded fraction), dark grey for the rest. Keyboard and screen-reader behaviour is unchanged (a native range input still does the work), and forced-colours mode shows the system slider.
- No more words popping into the player bar: dragging the volume or the seek bar no longer shows "Waiting to confirm…" or "Seek to …". The seek target shows in the elapsed time, the volume follows the hand until the player reports it, sliders lose their native tooltips, and a discrete command that is still unconfirmed after 600 ms shows as a quiet dot in the status chip (and beside a queue row) instead of text.

## 1.0.0-1

- Sidecar restart and startup recovery: after an unexpected `ytm-daemon` restart, the host notifies the renderer, discards stale epochs/in-flight requests, re-subscribes snapshot/delta state without replaying old writes, and rebuilds tray state; initial preload connection failures show a banner with bounded retries and manual reconnect.
- Concurrent UI and MCP settings: UI settings updates send field patches (`AppSettingsPatch`) merged under the same core lock as MCP updates and revert with explicit feedback on save failures.
- MCP compatibility and identity: `ytm-glass-mcp` reports product identity `"ytm-glass-mcp"` at `1.0.0` in `serverInfo` and includes bounded JSON payloads in `content[0].text` (and share URLs in share-link text) alongside `structuredContent` for content-only harnesses.
- UI pagination and keyboard navigation: the Add to playlist picker supports continuation loading and page-load retry; `Ctrl+F` awaits view-transition DOM readiness before focusing search; `VirtualizedTrackList` preserves a single visible `tabindex="0"` row after scrolling without stealing focus.
- MPRIS, autoplay, and recovery polish: global MPRIS `Play`/`Pause`/`Stop` act without binding a stale cached track instance; `autoplay_on_launch` applies once when the official page self-restores a paused track while manual control still wins; confirmed-unsent `Accepted` operations settle to terminal `Cancelled` (`NewPlanRequired`) on restart.
- Testing and release engineering: replaced blind bridge test `settle()` races with event-driven transport synchronization, unified workspace manifests and `packaging/PKGBUILD` to `1.0.0-1`, and added an offline GitHub Actions gate workflow.

## 0.1.0-20

- New icon set: 24 px stroke glyphs with filled active states; the play/pause button is a solid accent disc, and icon buttons follow the glass icon rules. The interface is English by default, with Simplified Chinese selectable in Settings.
- Account isolation: a different account signed in between two cookie reads now gets a new account epoch, and the catalog checks the session's own identity on every rebind instead of reusing the epoch's.
- Manual control and receipts: the queue reports no current entry while the page's selection lags behind the playing track, so selecting or interrupting can no longer be confirmed against the wrong track; launch restore waits for the page's first report and never restores another account's track; a failed page probe keeps the player stale until health recovers; consecutive plays of one song are separate history entries; clearing the cache fences off reads already in flight.
- Clearing and replacing upcoming entries are best effort: loaded upcoming entries are removed (the nearest 50) and the result says when more may still load.
- `--grant dj` also covers device settings and lyric offsets.
- DJ history counts only time actually played and starts over when the account changes; check-ins do not catch up after the DJ was off.
- MCP: a lost reply is looked up by its operation ID instead of being resent, a retried call with the same ID returns the original result, and playing a queue entry works without bookkeeping. A replacement stops when the cleared queue gains an entry, stay-on interruptions are fenced against a track change, and an account handoff waiting across a restart settles.
- MCP registration by tier: `--grant dj`, `record` and `edit`. The account defaults to the one signed in to the player, the credential file path stays the same when a client is registered again, and only edit-tier registrations expire (24 hours by default, 7 days at most). Granting playlist privacy changes is refused.
- Daily account limits per account over a rolling 24 hours: 100 items for the record tier, 20 for the edit tier with at most 10 per call. Counts survive restarts. Edit-tier removals record what they took away so it can be restored.
- Tool results speak in four plain outcomes (done, partly done, not done with a way out, unclear). Stale writes are returned to the agent instead of being retried. The operation journal tools are no longer published, leaving 39 tools.
- After a restart the player settles interrupted playlist creations, appends, ratings, subscriptions and library saves on its own from a fresh read.
- DJ: check-ins nobody answers stretch the gaps (up to eightfold) and are held to three per hour; waits return within about a minute for five minutes after either side speaks; prompts tell the agent to keep looping after it speaks; prompts are compact and send taste notes only on the first prompt and after a change.
- The UI language (English or Simplified Chinese) is a player setting saved with the others, so it survives restarts; `ytm_get_settings` shows it as `uiLanguage` and `ytm_update_settings` can change it. Settings saved before it existed load as English.
- Database schema v7 (the database is backed up before the migration).

## 0.1.0-19

- When the DJ is turned back on, recent tracks start from the current one; songs played while it was off are no longer reported as skipped.

## 0.1.0-18

- DJ prompts count only explicitly queued tracks: with autoplay on, the last queued track still gets the 75-second lead and the note that nothing follows.
- The MCP adapter retries a stale write only when it generated the operation ID itself.

## 0.1.0-17

- Agent DJ: one prompt before each track ends, and mid-track check-ins driven by a pseudo-random points heartbeat that grows with time since the last interaction and with track length, and waits after the listener interacts or a track changes. Four chattiness levels: off, quiet, normal, chatty. New tool `ytm_wait_dj_prompt` with MCP progress notifications while waiting. (0.1.0-16 was a development build of this work.)
- Write tools no longer need an operation ID, preconditions or a context token, and return once the result settles. All tools are published; `--grant dj` covers playback and the queue.
- Removed DJ plans, sessions, leases, the local runner and the game input path. Taste notes keep only long-term likes and avoids and need only the `dj` grant. The desktop activity view no longer shows DJ sessions.
- Playing an album after another context is now confirmed when the page's queue selection lags behind the loaded album.
- Database schema unchanged (v6).

## 0.1.0-15

- Continuous DJ refills also when the page's autoplay is on by default; the queue panel's "Clear upcoming" follows the page capability; unrelated writes no longer interrupt running catalog reads; appending a track already in a playlist is refused; writes refuse `VL` browse IDs; commands that were clicked but did not reach their target settle by observation.

## 0.1.0-14

- External control: interrupt-now with three resume policies (including resuming the interrupted track at its position), replacing upcoming entries, long-term DJ preferences and account handoff.
- "Add to the end" is verified when an autoplay tail is present; an append whose end cannot be proven is refused before it reaches the page.
- The page's autoplay switch in its new location (the player side panel) can be controlled; with autoplay off, entries can be removed, moved, cleared and replaced by occurrence.
- Commands the page refuses before running are recorded as refused; manual volume, mute and repeat changes no longer interrupt agent queue work.
- Local history edits reach the daemon (the socket encoding was wrong before).
- Player: album pages continue in album context; the queue panel can move, remove, clear upcoming and toggle autoplay; play is disabled without a track; clearing the cache waits for the real result; optional track-change notifications (off by default, silent, never take focus); interface scaling zooms the whole window; the tray recovers when plasmashell starts late or restarts; the playing track's cover is never evicted; MPRIS does not offer play without a track.
- The activity view lists and deletes DJ preferences and shows pending account requests.
- Database schema v6. The database is backed up before the first migration.

## 0.1.0-5 to 0.1.0-13

- Development builds of the MCP adapter and local control: typed search, collection playback, library membership with independent readback, queue edits, discovery and media reads, pagination fixes and the first DJ preferences.

## 0.1.0-4

- Fixes from a full code review: late mute confirmations are released, shared catalog reads cancel per caller, sign-out and account switches retire old account epochs and caches, MPRIS publishes only the current local cover, lyrics cache keys cover all fields and clear with the catalog, only the latest cookie read is forwarded, malformed UI and cover URLs return HTTP 400, the host escalates from graceful exit to SIGTERM and SIGKILL, and home and library pages recognize playlists by tile content.

## 0.1.0-3

- The official player page starts with the window's initial visibility. The core stops periodic checks when idle and not playing.
- Liked songs and ordinary playlists keep playable videos without a creator link and still show the creator's name.
- When LRCLIB returns both title fields, `trackName` is preferred; responses with only `name` are still accepted.

## 0.1.0-2

- Restoring the last position at startup seeks behind the muted output gate; a user's own play takes priority over the restore.
- A disconnected player bridge updates the core capabilities and the KDE media controls. Page crashes and network recovery have their own observation paths.
- Startup failures write a structured error and exit. The packaged daemon is found by Electron directly.
- The built-in "Liked music" playlist is filtered by its canonical playlist ID, in any interface language.
- The package carries the build source, lockfile hashes, runtime versions, system dependencies and third-party licenses.

## 0.1.0-1

- First Arch package: the official player page, the native interface, tray, MPRIS and the library.
