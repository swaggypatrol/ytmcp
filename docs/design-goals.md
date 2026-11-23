# MCP and AI DJ design goals

These are the goals the MCP server and the AI DJ are built against. [mcp.md](mcp.md) and [ai-dj.md](ai-dj.md) describe how the current code meets them; where it does not yet, [known-issues.md](known-issues.md) says so.

## Principles

1. **Strictness follows consequences.** Playback has no gate; the account has gates.
2. **Rigor lives in the code.** The agent states intent. The owner decides once, when connecting a client.
3. **No questions, but gates and a floor.** Grants and item limits stop an action before it happens; honest results and a recoverable journal catch it afterwards. Nothing asks for confirmation while running.
4. **Failure is cheap.** A failed playback action returns one plain sentence and one way out, and is never escalated to the listener.
5. **The listener always wins.** A key press beats the agent; chattiness changes with one sentence; grants can be revoked at any time.

YouTube Music is a subscription: a wrong song or a few extra songs cost nothing. The player does two things: it gives the agent tools and listening context, and it prompts the agent at good moments. What to play and what to talk about is between the agent and the listener.

## Four tiers

Tiers are set by what a mistake would cost, not by read versus write. No tier asks the listener anything while running.

| Tier | Includes | If it goes wrong | How it is governed |
| --- | --- | --- | --- |
| Read | search; viewing the queue, playlists, library and history | nothing happens | a registration is enough |
| Play | playback, queueing, interruptions, queue edits, volume, device settings, taste notes (stored locally only) | a wrong song, gone with one key press | no confirmations, no limits; manual priority and command fences are the safety net |
| Record | likes and dislikes, library saves, artist subscriptions, creating private playlists and adding to playlists the client created | a few extra entries in the account, each removable; in bulk they would disturb the library and recommendations | a separate grant, not included in `dj`; at most 100 items a day; beyond that, refused |
| Edit | adding to existing playlists, removing or editing their entries, renaming and describing them, deleting playlists, deleting local history | things are lost | a separate, time-limited grant (24 hours by default, 7 days at most) listing the allowed playlists; at most 20 items a day and 10 per call; the journal keeps the removed entries so they can be restored |

- Making a playlist public or private is left to the owner, in the player or on the website. No agent grant allows it.
- Limits count items: one like is 1, adding 30 songs is 30, removing 5 entries is 5. Counts survive restarts and are kept per account over a rolling 24 hours.
- With only `dj` (the play tier), every account write is refused.

## The core that does not change

Manual priority, command fences, confirmation by evidence, journaling before sending, the account consistency check, the private local socket, and `ytm_wait_dj_prompt` as the only delivery path. This is the strict half, and neither the listener nor the agent sees it. Changing any of it needs evidence from the real page.

Dropped for good: plans and approvals, session leases, budgets and call limits on playback, candidate allowlists, and confirmation pop-ups.

## A. Connecting

- One command registers a client, using the player's current account by default, with no database lookup.
- The play tier (`dj`) does not expire. Only the edit tier is time-limited.
- Registering the same client name again keeps the credential file path, so the agent's configuration does not change.
- The player is restarted once after the first registration.
- The agent client allows all YTMcp tools at once, for example `mcp__ytmcp__*` in Claude Code's `permissions.allow`. This does not loosen safety; the gates are in the player.

## B. What the agent sees

Internal vocabulary (operation IDs, preconditions, epochs, fences, the journal, phases) stays behind the MCP adapter.

- A result says one of four things: done; partly done (naming which items); not done, nothing changed (with one way out); unclear (look before deciding). The eight phases stay in the structured content for debugging.
- Writes need no operation ID, preconditions or context token. The adapter fills them in from the current state and returns once the result settles. A stale write is not retried automatically; it goes back to the agent.
- Journal tools (looking up, cancelling or resuming operations) are not published to agents. Writes whose outcome can be read back after a restart (new playlists, appends, ratings, subscriptions, library saves) are settled by the player itself.
- A refusal gives the way out in the same sentence, for example "replacing what comes next is not possible here; use play next instead".
- Playback is best effort: clearing and replacing what comes next should not require the whole queue to be loaded, should clear what it can and honestly say "partly done". The old refusals came from the player's own preconditions, not from the official page.

## C. Account gates

See the tier table. In addition:

- The edit-tier journal records each removed entry's song, position and playlist, enough to restore it.
- Beyond a limit, the write is refused, naming the tier, the limit and when room frees up. The listener is not asked.
- Runaway test: a script posing as a runaway agent sends 200 account writes to a test playlist; the account changes by no more than the limits, and the journal restores the original state.

## D. Conversation

- **Shorter waits while the conversation is hot.** For 5 minutes after the listener or the agent speaks, each wait returns within about 60 seconds, so the agent reads the listener's answer. Otherwise waits run to the next prompt.
- **Fewer check-ins when nobody answers.** After the agent speaks without an answer, the next check-in gap grows, and at most 3 check-ins happen in any 60 minutes without an answer. Any action by the listener (a key press, speaking) restores the normal pace. While unanswered, track-ending prompts also tell the agent to say less.
- **The loop does not break.** Prompts and tool descriptions tell the agent to call the wait tool again right after speaking instead of stopping for an answer; the answer arrives with a later result.
- **Compact prompts.** Tracks carry only the necessary fields; taste notes come with the first prompt and after a change; the text does not repeat the whole JSON. An hour or two of DJing should not fill the agent's context.

## Track-ending prompts

The agent gets one prompt before each track ends, a "continue" cue.

- 45 seconds before the end by default; 75 seconds when at most one track is queued after it, to leave time to search and queue. At most a third of a short track's length. Autoplay suggestions do not count as queued.
- None when the length is unknown or playback is paused. One per track instance.
- The prompt carries the current track and time left, the next few tracks, whether recent tracks finished or were skipped, how long the listener has been quiet, and the taste notes. The agent decides to queue, change style, say a line, or do nothing.

## Check-in heartbeat

The heartbeat does not fire at fixed intervals. Like the pseudo-random critical strike chance in some games, it accumulates points.

- While playing, every 15 seconds adds points and fires with probability coefficient × points. Without a hit, points keep growing, so the longer nobody has asked, the likelier a question becomes.
- While the current track is longer than 7 minutes, each tick adds another 0.5.
- When the listener interacts, points reset and the minimum gap starts again. Interaction includes using the player or media keys and the agent reporting that the listener spoke.
- No check-ins within 60 seconds of a track change or inside the track-ending window, so check-ins land mid-track.
- When nobody answers, the pace drops as described in D.
- How to ask and in what tone is up to the agent, taking into account the current and previous tracks, skips and the time of day. It may also decide not to ask.

| Chattiness | Coefficient | Minimum gap | Maximum gap | Mean first check-in after an interaction |
| --- | --- | --- | --- | --- |
| Quiet | 0.0006 | 8 min | 25 min | about 15 min |
| Normal (default) | 0.0016 | 4 min | 15 min | about 9 min |
| Chatty | 0.004 | 2.5 min | 9 min | about 5 min |
| Off | — | never | — | — |

Gaps count playing time only; the means come from 4000 simulations. The numbers are tuned for a listening session of one to two hours.

## Delivery

`ytm_wait_dj_prompt` is the only entry point, called in a loop.

- It returns at once when a prompt is ready; otherwise it waits up to `maxWaitSeconds` (20 minutes by default) and returns empty. Hot conversations shorten this as in D.
- With a progress token, the wait sends MCP progress every 20 seconds.
- A prompt that arrives while the agent handles the previous one is kept for the next call. A track-ending prompt whose track has ended is dropped; a check-in expires after 3 minutes.
- Each call reports `userSpoke` (the listener spoke since the previous call) and `spoke` (the agent spoke), and may set chattiness.
- "The DJ is on" means an agent is waiting. With no wait call in the last 3 minutes, nothing accumulates and no prompts are made. There is no fallback runner.

## Voice and environment

- Speech recognition, speech output and conversation are on the agent's side. The player has no voice input, hotkeys or recording, and does not pin the current track at the moment someone speaks; the agent decides what "this one" or "the last one" means.
- The listener is usually away from the screen, so prompts and questions travel through the agent's channel, never through player pop-ups.
- Time, weather, holidays and location are the agent's to find out, or to ask about. The next version adds a `moment` field to prompts with the local clock, the listener's chosen holiday region and city; weather stays with the agent (see [ai-dj.md](ai-dj.md#voice-and-environment)).

## Taste notes

The agent can note songs or artists the listener likes or wants to avoid, with a short remark. This belongs to the play tier, is stored only on the device, and needs only the `dj` grant. The listener can review and delete notes in the activity view.

## Acceptance: by experience, not by mechanism

1. A 15-minute DJ run of about three songs keeps its loop, with zero confirmations of any kind, including permission pop-ups in the agent client.
2. While the conversation is hot, the agent starts handling something the listener says within 60 seconds.
3. Pause or Next from the listener takes effect at once, and the agent does not take it back.
4. In 60 unanswered minutes, no more than 3 check-ins.
5. The runaway test (section C) passes; with only `dj`, every account write is refused.
6. Going from nothing to music takes at most 3 steps without a database lookup, and it still works a week later without any action.

## Dropped designs

These came from earlier plans or implementations and were removed along with their code and tools:

- DJ plans with approval and execution, persistent DJ sessions with write leases (low-water marks, batches, track and time budgets, session generations), a background event runner with a candidate selector, refill tickets, candidate allowlists and call or interval limits.
- A game-mode input path that pinned the track at the moment of input.
- Session-scoped preferences and skip reasons. Taste notes no longer need the `preferences` grant, which still covers device settings and lyric offsets.
- Publishing only six read-only tools by default, and requiring every write to carry its own operation ID and preconditions.
- Automatic retry of stale writes: it could press play again for the agent right after the listener pressed pause.
