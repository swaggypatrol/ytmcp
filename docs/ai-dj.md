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

- **The DJ is on while an agent waits.** With no wait call for 3 minutes, nothing accumulates, no prompts are made and background sampling stops. There is no background runner.
- **One shared state.** Chattiness, the check-in heartbeat, recent tracks and the listener's last interaction are shared by all agents; only which prompt each client has already received is kept per client. Recent tracks are recorded only while the DJ is on, so songs played while it was off do not appear. All of this lives in memory and starts over when the player restarts.
- **Missed prompts.** A prompt that arrives while the agent is busy is kept for its next call, up to 8; older ones are dropped. A track-ending prompt expires when its track ends, a check-in after 3 minutes.
- **Keepalive.** If the call carries an MCP `progressToken`, the adapter sends a progress notification every 20 seconds while it waits, which keeps clients with short per-call timeouts waiting.

### Conversation-hot waits

For 5 minutes after either `userSpoke` or `spoke` is reported, each wait returns within about 60 seconds, so the agent reads the listener's reply soon. Many MCP clients hold a message the user sends during a tool call until the call returns; the short waits bound that delay. At other times waits run to the next prompt as usual.

### Loop continuity

Prompts and the tool description tell the agent to call `ytm_wait_dj_prompt` again right after speaking instead of stopping for an answer; the listener's reply arrives with a later result. In clients such as Claude Code, an assistant turn that ends with text and no tool call ends the turn, so this instruction is what keeps the DJ running.

## Track-ending prompts

Before each track ends the agent gets one prompt, which works as a "continue" cue.

- **Lead time.** 45 seconds before the end, or 75 seconds when at most one track is queued after it, to leave time to search and queue. For a short track the lead is at most a third of its length.
- **Only the explicit queue counts.** When the explicit queue is fully loaded, the page's autoplay suggestions are not counted as queued tracks. Only a truncated window or an open-ended queue such as a radio counts as "more queued". When at most one track is queued and autoplay is on, the prompt says that YouTube Music would continue on its own.
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

## What a prompt contains

| Field | Contents |
| --- | --- |
| `kind`, `reasons` | `trackEnding` or `checkIn`, and why (`littleQueued` means at most one track is queued) |
| `prompt` | The instruction for the agent, in English |
| `nowPlaying` | The current track, playback state, position and time left |
| `upNext`, `moreQueued` | Up to 5 upcoming tracks, and whether more follow |
| `recent` | Up to 8 recent tracks, newest first, with how long each played and how it ended: `finished`, `skippedByListener`, `skippedByAgent` or `skipped` |
| `listener` | How and how long ago the listener last interacted, when the agent last spoke, and how many check-ins went unanswered |
| `chattiness` | The current level |
| `preferences`, `preferencesUnchanged` | The recorded taste notes, sent on a client's first prompt and again only after they change |

Prompts are kept compact so an hour or two of DJing does not fill the agent's context. Tracks carry only video ID, title, artist names, album and length. The text content is a short summary (the instruction, now playing, up to 5 next and 4 recent tracks, the listener's idle time and changed tastes) rather than a copy of the JSON.

**How a track's end is classified.** A track that stops within 20 seconds of its end, or whose length is unknown, finished. If it ended earlier, a manual action within 8 seconds before the change makes it skipped by the listener, an agent write makes it skipped by the agent, and otherwise it was skipped for an unknown reason.

## Taste notes

`ytm_set_dj_feedback` records or clears a `prefer` or `avoid` note for a video or an artist, with an optional note of up to 200 characters. Notes are stored locally in SQLite per account, at most 500 per account; recording the same subject again replaces the old note. They never change likes or dislikes on YouTube and need only the `dj` grant. `ytm_get_settings` with section `djPreferences` pages them, and the activity view in the interface lists and deletes them.

## Voice and environment

Speech recognition, speech output and the conversation itself belong to the agent. The player has no voice input, hotkeys or recording, and does not pin "this song" to the moment someone spoke; the agent decides what "this one" means from the context. The time, weather, holidays and location are the agent's to find out, or to ask about. The listener is often away from the screen, so the player never relies on pop-ups.

**Planned for the next version.** Prompts gain a small `moment` field so the agent can shape a set around the day without being told: local time, weekday, part of the day and time zone from the machine clock; public holidays and their eves for a region the listener picks; and a city the listener types once in Settings (no GPS or IP lookup; empty means the agent is never told). Weather stays with the agent: it can look the forecast up for that city with its own tools, and the player makes no extra outside requests for it. This is not in 1.0.

## Setup example

```sh
ytm-glass-mcp register --principal claude-dj --grant dj
# Restart YTM Glass once after the first registration. Add --grant rating to let it like songs.
claude mcp add ytm -s user -- /usr/bin/ytm-glass-mcp \
  --credential-file ~/.config/ytm-glass/control-credentials/claude-dj.json --allow-background-start
```

Then tell the agent something like "Be my DJ tonight, something jazzy." A typical session:

1. The agent searches, starts an album and calls `ytm_wait_dj_prompt` with `userSpoke`.
2. With 45 seconds left on the first track it gets a track-ending prompt: four tracks queued, nothing skipped. It decides to change nothing and waits again.
3. The second track is nine minutes long. Midway a check-in fires with reason `longTrack`, and the agent asks in one line whether the mood still fits, then waits again at once.
4. The listener answers "a bit slow, something lighter". The agent replaces what comes next with `ytm_enqueue` or `ytm_interrupt_with`, perhaps records a taste note, and waits with `userSpoke` and `spoke`.
5. The listener presses Next. The track is recorded as skipped by the listener, the next prompt shows it, and the heartbeat starts over.
