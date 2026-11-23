import type { PlaybackStatus } from "@ytmcp/protocol";

export interface MotionPolicyInput {
  readonly reduceMotionSetting: boolean;
  readonly prefersReducedMotion: boolean;
  readonly documentHidden: boolean;
  readonly playbackStatus: PlaybackStatus;
}

/**
 * Suspends decorative animations and background visual loops when:
 * 1. User enabled `reduceMotion` setting or OS prefers-reduced-motion
 * 2. UI window is hidden/minimized (`documentHidden === true`)
 * 3. Playback is paused/idle/ended
 */
export function shouldRunDecorativeAnimation(input: MotionPolicyInput): boolean {
  if (input.reduceMotionSetting || input.prefersReducedMotion || input.documentHidden) {
    return false;
  }
  return input.playbackStatus === "playing" || input.playbackStatus === "buffering";
}
