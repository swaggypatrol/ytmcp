/** Never sooner than this, so a boundary that was just reached cannot spin the timer. */
const MIN_TICK_MS = 16;
/** Lands just past the boundary, so the rounded position read then is already over it. */
const TICK_SLACK_MS = 8;
/**
 * Never later than this, so the seek bar's fill keeps moving in sub-pixel steps (a four-minute
 * track on an 800 px bar moves about 0.7 px per wake) instead of a visible jump each second.
 */
export const MAX_TICK_MS = 200;

/**
 * Delay until the shown playback position next needs updating: its next whole second, or the
 * next lyric line start in `lineTimesMs` (ascending, display times) if that comes sooner, and
 * never more than MAX_TICK_MS for the seek fill. Every wake computes from the position it reads,
 * so the seconds and the lines still change exactly on time. The delay is in wall time, so it is
 * divided by the playback rate. App schedules one timer at a time with it, only while the
 * position is being extrapolated and the window is visible.
 */
export function nextPositionTickMs(positionMs: number, playbackRate: number, lineTimesMs: readonly number[] = []): number {
  const rate = playbackRate > 0 ? playbackRate : 1;
  let target = (Math.floor(positionMs / 1000) + 1) * 1000;
  for (const lineMs of lineTimesMs) {
    if (lineMs >= target) break;
    if (lineMs > positionMs) {
      target = lineMs;
      break;
    }
  }
  const toTarget = Math.ceil((target - positionMs) / rate) + TICK_SLACK_MS;
  return Math.max(MIN_TICK_MS, Math.min(MAX_TICK_MS, toTarget));
}
