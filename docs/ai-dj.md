# AI DJ

The player does not pick music. It hands an agent the tools and the listening context, and tells it when there is something worth reacting to. What to play, what to say and whether to do anything at all is up to the agent. The reasoning behind this split is in [design-goals.md](design-goals.md); the code is in `crates/core/src/control/cues.rs` and `crates/mcp/src/main.rs`.

## The loop

The agent calls `ytm_wait_dj_prompt` in a loop. Each call returns when a prompt is ready, on an error or cancellation, or after `maxWaitSeconds` (default 1200, at most 1800) with no prompt. After acting on a prompt, or deciding not to, the agent calls again.

| Argument | Meaning |
| --- | --- |
| `userSpoke` | The listener talked to the agent since its previous call. |
| `spoke` | The agent said something to the listener since its previous call. |
| `chattiness` | `off`, `quiet`, `normal` (default) or `chatty`. It stays set until changed or the player restarts. |
| `maxWaitSeconds` | 1 to 1800. |
| `lastReceived` | `{coreEpoch, seq}` of the last prompt the client received. A prompt handed to a wait the client cancelled before its reply arrived is offered again; one naming another core epoch is ignored. Skip repeats by `(coreEpoch, seq)`. |

- **The DJ is on while an agent waits.** With no wait call for 3 minutes, nothing accumulates, no prompts are made and background sampling stops. There is no background runner.
- **One shared state.** Chattiness, the check-in heartbeat, recent tracks, the listener's last interaction and their recent actions are shared by all agents; only which prompts and which listener actions each client has already received are kept per client. Recent tracks are recorded only while the DJ is on, so songs played while it was off do not appear. All of this lives in memory and starts over when the player restarts.
- **Missed prompts.** A prompt that arrives while the agent is busy is kept for its next call, up to 8; older ones are dropped. A track-ending prompt expires when its track ends, a check-in or listener signal after 3 minutes. A prompt counts as received when a poll hands it out, so a client that cancels waits passes `lastReceived` to get back one its cancelled wait took.
- **Keepalive.** If the call carries an MCP `progressToken`, the adapter sends a progress notification every 20 seconds while it waits, which keeps clients with short per-call timeouts waiting.

### Conversation-hot waits

For 5 minutes after either `userSpoke` or `spoke` is reported, each wait returns within about 60 seconds, so the agent reads the listener's reply soon. Many MCP clients hold a message the user sends during a tool call until the call returns; the short waits bound that delay. At other times waits run to the next prompt as usual.

### Loop continuity

Prompts and the tool description tell the agent to call `ytm_wait_dj_prompt` again right after speaking instead of stopping for an answer; the listener's reply arrives with a later result. In clients such as Claude Code, an assistant turn that ends with text and no tool call ends the turn, so this instruction is what keeps the DJ running.

## Track-ending prompts

Before each track ends the agent gets one prompt, which works as a "continue" cue.

- **Lead time.** 45 seconds before the end, or 75 seconds when at most one track is queued after it, to leave time to search and queue. For a short track the lead is at most a third of its length.
- **Only the explicit queue counts.** When the explicit queue is fully loaded, the page's autoplay suggestions are not counted as queued tracks. Only a truncated window or an open-ended queue such as a radio counts as "more queued". The prompt states what YouTube Music does on its own after the queued tracks, as read from the page: when at most one track is queued and suggestions are loaded, it names the first three and the selected Up Next filter and says the agent may leave them or steer them with another filter (up to four other labels, such as Discover or Popular); it also says when autoplay is off, when the queue is a radio that refills itself, or when the page is still loading suggestions. When the page's continuation cannot be read, it falls back to the autoplay setting.
- **Steering the suggestions.** When the listener asks for "something more familiar" or "something new" and YouTube Music's own suggestions follow, the agent can select an Up Next filter with `ytm_set_playback_mode` (`upNextFilter` with a `filterId` from `ytm_get_context` `queue.filters`) instead of queueing tracks itself. Only the suggestions after the queued tracks change; the current track keeps playing and queued tracks stay.
- **When there is none.** One prompt per track instance; none while paused or when the length is unknown. If the DJ turns on inside the lead window, the prompt comes at once with less lead.

## Check-in heartbeat

Between track endings, a heartbeat decides when it is a good moment to ask the listener how the music is going. It works like a pseudo-random distribution: the longer nobody has asked, the likelier a question becomes.

1. **Points.** While music plays (not while paused or buffering), every 15 seconds adds points: 1 normally, 1.5 when the listener has been idle for over 12 minutes, 2 after 25 minutes, plus 0.5 while the current track is longer than 7 minutes.
2. **Roll.** Each tick fires with probability *coefficient × points*. If it does not fire, points keep growing. Gaps count playing time only; nothing fires before the minimum gap, and one always fires at the maximum.

   | Chattiness | Coefficient | Minimum gap | Maximum gap | Mean first check-in after an interaction |
   | --- | --- | --- | --- | --- |
   | `quiet` | 0.0006 | 8 min | 25 min | about 15 min |
   | `normal` | 0.0016 | 4 min | 15 min | about 9 min |
   | `chatty` | 0.004 | 2.5 min | 9 min | about 5 min |
   | `off` | — | — | — | never |

   The means come from 4000 simulated gaps; a unit test keeps them in range.

3. **Quiet windows.** No check-in from the start of a track's ending window until 60 seconds into the next track (turning the DJ on counts as a track change). Points still accumulate, so a check-in that came due fires right after the window.
4. **Resets.** Any listener action in the interface, tray, media keys or MPRIS (volume included), or `userSpoke`, resets the points and the idle clock. `spoke` resets the points but not the idle clock.
5. **Fewer check-ins when unanswered.** A check-in counts as unanswered as soon as it fires. Each unanswered check-in doubles the minimum and maximum gaps, up to 8 times the normal gaps, and at most 3 check-ins fire in any hour without an answer. Track-ending prompts also tell the agent to stay quiet unless something matters. Any listener action or `userSpoke` restores the normal pace.
6. **Reasons.** Each check-in lists why it fired: `listenerQuiet` (idle for over 12 minutes), `longTrack`, or `routine`. How to ask, in what tone, or whether to ask at all is the agent's choice.

## Listener signals

The third kind of prompt, `listenerSignal`, comes a few seconds after the listener steers the music by hand, so the agent can adjust what plays next. It is a chance to change the music, not a prompt to speak. Four things fire it, each named in `reasons`:

| Reason | What the listener did |
| --- | --- |
| `skipStreak` | Skipped two or more tracks in a row, all within 3 minutes of the last, each more than 20 seconds before its end (or of unknown length) |
| `manualPick` | Started a track, an album or playlist, or an entry of the queue |
| `replay` | Went back to hear a track again (pressed previous at least 10 seconds into it, or at the very start of the next one after hearing at least 10 seconds), or turned on repeat-one |
| `filterChanged` | Chose another Up Next filter |

- **Never for the rest.** Pausing, stopping, the volume, mute, seeking, likes, shuffle, autoplay and queue additions, removals and moves never fire one; they appear only in the next prompt's `recentActions`.
- **One per burst.** The signal waits until 5 seconds after the listener's last action, so a burst of clicks is one prompt that describes up to three of them in words. At most one signal comes every 2 minutes; a burst inside that gap is held, keeps growing and fires when the gap ends. Turning repeat-one on and off again within a burst fires nothing.
- **Only while an agent waits.** Only actions made while the DJ is on count. A signal expires after 3 minutes, like a check-in.
- **Not about talking.** It arrives at every chattiness, `off` included, and does not count as an unanswered check-in. The prompt says not to undo what the listener just chose and to say nothing unless it adds something. The listener's own controls still always win.

## What a prompt contains

| Field | Contents |
| --- | --- |
| `kind`, `reasons` | `trackEnding`, `checkIn` or `listenerSignal`, and why (`littleQueued` means at most one track is queued; the signal reasons are above) |
| `coreEpoch` | Current core daemon instance identifier |
| `prompt` | The instruction for the agent, in English |
| `nowPlaying` | The current track, playback state, position and time left |
| `upcoming`, `upNext`, `moreQueued` | Whether what follows the current track is known, up to 5 upcoming tracks, and whether more follow. When the current track is not in the loaded queue window, `upcoming` is `unknown` and `upNext` is empty: that is not an empty queue, and the prompt says so |
| `officialUpNext` | What YouTube Music plays on its own after the queued tracks: `status` (`observed`, `loading`, `off`, `radio` or `none`), up to 5 of its suggestions, whether more are loaded, and the selected Up Next filter. Read-only and never counted as queued. Absent when the page state was not readable |
| `recent` | Up to 8 recent tracks, newest first, with how long each played and how it ended: `finished`, `skippedByListener`, `skippedByAgent` or `skipped` |
| `listener` | How and how long ago the listener last interacted, when the agent last spoke, and how many check-ins went unanswered. `recentActions` lists what the listener did by hand since this client's previous prompt (a client's first prompt looks back 10 minutes), newest first, at most 12, and `actionsOmitted` counts older ones left out. Only actions in the window, tray, media keys and MPRIS count, plus likes and playlist additions made in the app; commands from agents never do. Each action is handed to a client once, a volume or seek drag still going on waits for the next prompt, and a prompt lost with a cancelled wait gives its actions back through `lastReceived`. They are facts, such as the volume going from 70 to 30 or a track skipped 12 seconds in; what they mean is the agent's call |
| `chattiness` | The current level |
| `preferences`, `preferencesUnchanged`, `preferencesTotal`, `preferencesUnavailable` | Recorded taste notes (the newest 50, on a client's first prompt and again after they change), how many are stored in all, and whether reading them failed this time |
| `moment` | Privacy-gated Moment DJ view (`environment` and active temporary `listeningIntents`) for that client |

Prompts are kept compact so an hour or two of DJing does not fill the agent's context. Tracks carry only video ID, title, artist names, album and length. The text content is a short summary (the instruction, now playing, up to 5 next tracks, one line on YouTube Music's own continuation, 4 recent tracks, the listener's idle time, one line naming their newest six actions in plain words and counting the rest, and changed tastes) rather than a copy of the JSON.

**How a track's end is classified.** A track that stops within 20 seconds of its end, or whose length is unknown, finished. If it ended earlier, a manual action within 8 seconds before the change makes it skipped by the listener, an agent write makes it skipped by the agent, and otherwise it was skipped for an unknown reason.

## Taste notes

`ytm_set_dj_feedback` records or clears a `prefer` or `avoid` note for a video or an artist, with an optional note of up to 200 characters. Notes are stored locally in SQLite per account, at most 500 per account; recording the same subject again replaces the old note. They never change likes or dislikes on YouTube and need only the `dj` grant. `ytm_get_settings` with section `djPreferences` pages them, and the activity view in the interface lists and deletes them.

## The listener's own music

`ytm_get_listener_profile` sums up what the listener already has: up to 200 liked songs, up to 200 plays this player recorded, their first 30 playlists and the number of taste notes, with how much of each was read, the artists that appear most, the newest tracks and the playlists by name. It also gives the writing scripts of the titles and artist names, such as "Latin 91%, Kana 6%, Han 3% (from 240 tracks)". It is a read like any other, needs no extra consent and is not part of Moment DJ.

- **Their music, not your language.** The scripts describe what the listener listens to, not their language, country or interface language, and the player infers nothing from the interface language, time zone, Moment settings or the conversation. The server instructions and the tool's description ask the agent to read the profile before it first chooses music in a session, and to choose within the scripts and styles the library shows rather than leaning toward any language, country or scene because of its training or the language of the conversation. A listener may talk to the agent in one language and listen in another.
- **Then ask or look.** When it is still unclear what the listener wants, the agent can ask them, or read further into their library and history with the read tools. What they do with the controls always wins.
- **Honest gaps.** A part that could not be read is listed in `unavailable`, never shown as empty. Liked songs and playlists are kept for 15 minutes per account session (`refresh` reads them again); plays and taste notes are read on every call.

## Voice and Moment DJ

Speech recognition, speech output and the conversation itself belong to the agent. The player has no voice input, hotkeys or recording, and does not pin "this song" to the moment someone spoke; the agent decides what "this one" means from the context. The listener is often away from the screen, so the player never relies on pop-ups.

**Moment DJ (`moment`).** Both `ytm_get_context` and `ytm_wait_dj_prompt` include a `moment` object with two parts, `environment` and `listeningIntents`. Their text content says the same in a line or two, including "not shared" and "none active", so a client that reads only text gets the same answer:

- **Off until you share it.** Every assistant starts with both parts `notShared`. In **Settings → Moment DJ context** you choose what is shared (the clock, holidays for a region you pick, a mood city) and which assistants of the signed-in account get it. A preview shows, in plain words and as the exact JSON, what the chosen assistant receives.
- **Offline, no lookups.** The desktop app works out the local date, weekday, part of the day (`earlyMorning` to `lateNight`) and time zone from the machine clock, and holidays from the `date-holidays` calendar built into the app. On the day before a public holiday the holiday is listed with phase `eve`. `capturedAtMs` is the instant `time` describes, so an assistant can turn "tonight" into an `until` time. The mood city is a label the listener typed, not where they are, and the player never fetches weather (`weatherProvided` is always `false`); an assistant that wants a forecast looks it up itself.
- **Each part fails on its own.** If the holiday calendar fails, the clock is still shared and only `occasions` is missing. `unavailable` means nothing could be read this time, which is not the same as "nothing".
- **Temporary listening intents (`ytm_set_listening_intent`).** An assistant records what the listener asked for right now: vocals, energy, an activity (`gaming`, `focus`, `relax`, `social`), a short music brief, or a holiday to leave out (`dismissTheme` with the occasion's id). Each lasts `today` (until local midnight, fixed when it is written), a 6-hour `session`, or `until` a time within 48 hours, and is never turned into a permanent taste. An intent the assistant only inferred never replaces one the listener asked for, and must name the Moment revision it was decided on (`expectedMomentRevision`): if the listener changed something since, the write is refused and the assistant decides again on a fresh read. Clearing with the intent's `intentId` leaves an intent that replaced it in the meantime. A holiday dismissed for today stays out of `occasions` even while intents are not shared. The DJ activity view lists every active intent with when it ends, and the listener can clear one or all.

## Setup example

```sh
ytmcp register --principal claude-dj --grant dj
# Restart YTMcp once after the first registration. Add --grant rating to let it like songs.
claude mcp add ytmcp -s user -- /usr/bin/ytmcp \
  --credential-file ~/.config/ytmcp/control-credentials/claude-dj.json --allow-background-start
```

Then tell the agent something like "Be my DJ tonight, something jazzy." A typical session:

1. The agent reads `ytm_get_listener_profile`, searches within what the listener already plays, starts an album and calls `ytm_wait_dj_prompt` with `userSpoke`.
2. With 45 seconds left on the first track it gets a track-ending prompt: four tracks queued, nothing skipped. It decides to change nothing and waits again.
3. The second track is nine minutes long. Midway a check-in fires with reason `longTrack`, and the agent asks in one line whether the mood still fits, then waits again at once.
4. The listener answers "a bit slow, something lighter". The agent replaces what comes next with `ytm_enqueue` or `ytm_interrupt_with`, perhaps records a taste note, and waits with `userSpoke` and `spoke`.
5. The listener presses Next. The track is recorded as skipped by the listener, the next prompt shows it, and the heartbeat starts over.
