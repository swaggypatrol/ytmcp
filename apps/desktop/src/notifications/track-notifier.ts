import type { AuthState, PlayerObservedState } from "@ytm-glass/protocol";
import { Notification } from "electron";

/** Rapid skips collapse into one notification for the track that is current once this settles. */
export const TRACK_NOTIFICATION_DEBOUNCE_MS = 1_500;

export interface TrackNotifierOptions {
  /** Whether the user is looking at the main window; nothing is shown while it is focused. */
  readonly isMainWindowFocused: () => boolean;
  readonly debounceMs?: number;
}

interface TrackCandidate {
  readonly key: string;
  readonly title: string;
  readonly body: string;
}

function candidateFrom(player: PlayerObservedState | null | undefined): TrackCandidate | null {
  const track = player?.currentTrack;
  if (!track || typeof track.title !== "string" || track.title.trim() === "") {
    return null;
  }
  // One key per play of a track: a repeated song in the queue is a new instance, a snapshot
  // rebuild of the same play is not.
  const key = player?.trackInstanceId
    ? `instance:${player.trackInstanceId}`
    : `video:${track.videoId}:${player?.currentQueueEntryId ?? ""}`;
  const body = (track.artists ?? [])
    .map((artist) => artist.name)
    .filter((name) => typeof name === "string" && name.trim() !== "")
    .join(", ");
  return { key, title: track.title, body };
}

/**
 * Shows a silent, passive desktop notification when the core-reported track changes while
 * the user has track notifications on, is signed in and is not looking at the main window.
 *
 * The notification has no actions, no click handler and never requires interaction, so it
 * can neither raise a window nor take focus (the app runs alongside fullscreen games).
 * Only metadata already in the core snapshot is used; no cover is fetched.
 */
export class TrackNotifier {
  private readonly debounceMs: number;
  private enabled = false;
  private authState: AuthState | null = null;
  private current: TrackCandidate | null = null;
  /** The first observation is the baseline: a track already playing at startup is not announced. */
  private observedPlayer = false;
  private lastNotifiedKey: string | null = null;
  private timer: NodeJS.Timeout | null = null;
  private shown: Notification | null = null;

  public constructor(private readonly options: TrackNotifierOptions) {
    this.debounceMs = options.debounceMs ?? TRACK_NOTIFICATION_DEBOUNCE_MS;
  }

  public setEnabled(enabled: boolean): void {
    this.enabled = enabled;
    if (!enabled) {
      this.cancel();
      this.closeShown();
    }
  }

  public setAuthState(state: AuthState | null | undefined): void {
    this.authState = state ?? null;
    if (this.authState !== "authenticated") {
      this.cancel();
      this.closeShown();
    }
  }

  public observePlayer(player: PlayerObservedState | null | undefined): void {
    const next = candidateFrom(player);
    const baseline = !this.observedPlayer;
    this.observedPlayer = true;
    const sameTrack = next?.key === this.current?.key;
    // Later observations of the same play may correct title/artists; show the latest ones.
    this.current = next;
    if (sameTrack) {
      return;
    }
    if (baseline) {
      // Never announce the track found at startup or before the first state arrived.
      this.lastNotifiedKey = next?.key ?? null;
      return;
    }
    this.cancel();
    if (next && next.key !== this.lastNotifiedKey) {
      this.timer = setTimeout(() => this.fire(), this.debounceMs);
      this.timer.unref?.();
    }
  }

  public dispose(): void {
    this.cancel();
    this.closeShown();
  }

  private fire(): void {
    this.timer = null;
    const candidate = this.current;
    if (
      !candidate ||
      !this.enabled ||
      this.authState !== "authenticated" ||
      candidate.key === this.lastNotifiedKey
    ) {
      return;
    }
    // Settled on this track: whether shown or seen in the focused window, it is never announced again.
    this.lastNotifiedKey = candidate.key;
    if (this.options.isMainWindowFocused() || !Notification.isSupported()) {
      return;
    }
    this.closeShown();
    const notification = new Notification({
      title: candidate.title,
      body: candidate.body,
      silent: true,
    });
    this.shown = notification;
    notification.on("close", () => {
      if (this.shown === notification) {
        this.shown = null;
      }
    });
    notification.show();
  }

  private cancel(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  private closeShown(): void {
    const shown = this.shown;
    this.shown = null;
    shown?.close();
  }
}
