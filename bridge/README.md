# `@ytmcp/bridge` — YouTube Music page bridge

`bridge/` is the only module allowed to depend on YouTube Music DOM structure, private page
objects and page-side request shapes. There is exactly one shipped bridge.

## Bundles (built by `pnpm --filter @ytmcp/bridge run build`)

| Source | Output | World | Role |
| --- | --- | --- | --- |
| `src/bridge.ts` | `dist/bridge-probe.mjs` | isolated world 1001 (`BRIDGE_WORLD_ID`) | Observation, queue mirror, command dispatch, capabilities, envelopes to the host transport |
| `src/page-shim.ts` | `dist/page-shim.iife.js` | page main world | Fixed allowlisted adapter to the player API, redux queue state, page controls and `ytcfg` |

The output file names are what `apps/desktop/src/rust-host/bridge-injector.ts` loads; the
`-probe` suffix is historical. `BRIDGE_VERSION` (`src/types.ts`) is reported in `bridge.ready`.

## Data sources

- **Time**: `movie_player.getCurrentTime()` and `getPlayerResponse().videoDetails.lengthSeconds`.
  These are per track. The `<video>` element shares one MSE timeline across gapless tracks
  (its `currentTime`/`duration` accumulate), so it is used only for change events and as a
  fallback when the player API is missing. Seeks use `seekTo` in the same per-track seconds.
- **Playback instance**: the player's per-load nonce (`getCpn()`); it changes on every new
  load, including gapless transitions and duplicate occurrences, and stays the same when
  YouTube Music's "previous" restarts the track, so the core sees the position jump back.
- **Queue**: `ytmusic-player-bar.getState().queue` (`items`, `selectedItemIndex`,
  `isInfinite`, `automixItems`, `shuffleEnabled`, `repeatMode`). Entry ids are
  `qe:<bridgeEpoch>:<n>`; an id survives a snapshot only when the page occurrence key
  (`videoId|playlistSetVideoId|watchEndpoint.index`) is unique before and after. Snapshots
  carry at most 200 entries (`loadedRange`).
- **Account**: `ytcfg.get("DATASYNC_ID")` → `acct:` + first 32 hex of
  SHA-256(`"ytm-glass-account-v1\n" + id`), computed in the shim; the raw id never leaves the page.

## Commands

| op | Page mechanism | Notes |
| --- | --- | --- |
| play / pause / toggle | `playVideo` / `pauseVideo` | toggle uses latest observation |
| seek | `seekTo(sec)` | clamped to the track duration |
| next / previous | player-bar buttons (API fallback) | previous > 3 s restarts the track (page behavior) |
| setVolume / setMuted | `setVolume(0..100)` / `mute`/`unMute` | protocol volume is 0..1 |
| setShuffle / setRepeat | player-bar buttons until the store matches | absolute targets |
| playTarget | `ytmusic-app` in-page navigation (`watchEndpoint`), then `playVideo` once the target is loaded but not playing | ids validated |
| selectQueueEntry | play button of the matching `#contents` row | occurrence key re-checked |
| enqueue next / last | the page's own queue-add behavior (`Play next` / `Add to queue`) | `last` on an endless radio is rejected: the page returns nothing for it |
| stop, queue reorder/remove | — | `UNSUPPORTED_CAPABILITY` |

`bridge.commandAck.dispatched` only means the page call ran; the core confirms effects from
later observations.

## Cadence (SPEC 9.3)

Event driven: media events and debounced MutationObservers trigger a shim probe; at most one
probe is in flight. `timeupdate` is sampled at most once per second while playing. A health
heartbeat runs every 12 s. There is no per-frame or fast polling. The bridge has no signal
for main-window visibility, so cadence does not depend on it.

## Checks

```bash
pnpm --filter @ytmcp/bridge run typecheck
pnpm --filter @ytmcp/bridge test
pnpm --filter @ytmcp/bridge run build
# Live (real profile, audio on a temporary null sink; see docs/building.md)
YTMCP_ALLOW_LIVE=1 node tools/probes/bridge/run-live-bridge-probe.mjs
```
