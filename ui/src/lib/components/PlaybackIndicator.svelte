<script lang="ts" module>
  import type { PlaybackStatus, PlayerConnectionState } from "@ytmcp/protocol";

  export type IndicatorState = "playing" | "paused" | "buffering";

  /** What the indicator shows for a playback status; `null` means it is not shown at all. */
  export function indicatorState(
    playback: PlaybackStatus | null | undefined,
    connection?: PlayerConnectionState | null
  ): IndicatorState | null {
    if (connection === "disconnected" || connection === "incompatible") return null;
    if (playback === "playing" || playback === "buffering") return playback;
    if (playback === "paused" || playback === "ended") return "paused";
    return null;
  }
</script>

<script lang="ts">
  interface Props {
    playback: PlaybackStatus | null | undefined;
    connection?: PlayerConnectionState | null;
    /** The app's decorative motion policy (motion allowed and the window visible); static bars otherwise. */
    motion: boolean;
    class?: string;
  }

  let { playback, connection = null, motion, class: className = "" }: Props = $props();

  const mode = $derived(indicatorState(playback, connection));
  const animated = $derived(motion && mode !== "paused");
</script>

<!-- Three short bars: animated while playing, still while paused, a slow pulse while buffering. -->
{#if mode}
  <span
    class="playback-indicator {className}"
    class:is-animated={animated}
    data-state={mode}
    data-animated={animated ? "true" : "false"}
    aria-hidden="true"
    data-testid="playback-indicator"
  >
    <span class="playback-indicator-bar"></span>
    <span class="playback-indicator-bar"></span>
    <span class="playback-indicator-bar"></span>
  </span>
{/if}
