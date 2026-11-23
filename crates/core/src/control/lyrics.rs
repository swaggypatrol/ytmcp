use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, LyricsInput, LyricsOffsetAction, LyricsPage,
    LyricsSourcePreference, MAX_CONTENT_BYTES, OperationPhase, OperationResource, RetryDisposition,
    TrackTarget, VerificationLevel, WriteAction,
};
use ytm_protocol::{
    LyricsKind, LyricsMatchMetadata, LyricsQueryRequest, LyricsResult, QueryError, Track, VideoId,
};

use super::write::{MusicOperation, Transition, permission};
use super::{ControlRead, ControlService, ReadGrant, invalid, map_query_error, stale, unavailable};
use crate::ports::BoxFuture;

const DEFAULT_LINES: u16 = 40;
const MAX_LINES: u16 = 200;
const MAX_OFFSET_MS: i64 = 10_000;
const CURSOR_OVERHEAD: usize = 96;

pub trait ControlLyrics: Send + Sync {
    fn read_lrclib(
        &self,
        track: LyricsMatchMetadata,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<LyricsResult, QueryError>>;

    fn user_offset(&self, video_id: VideoId) -> BoxFuture<'_, Result<i64, QueryError>>;

    fn change_offset(
        &self,
        video_id: VideoId,
        action: LyricsOffsetAction,
    ) -> BoxFuture<'_, Result<i64, QueryError>>;
}

impl ControlService {
    pub(super) async fn read_lyrics(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        input: LyricsInput,
    ) -> Result<LyricsPage, ControlError> {
        let cursor = input.page.as_ref().and_then(|page| page.cursor.as_deref());
        let limit = input
            .page
            .as_ref()
            .and_then(|page| page.limit)
            .unwrap_or(DEFAULT_LINES);
        if !(1..=MAX_LINES).contains(&limit)
            || cursor.is_some_and(|cursor| cursor.is_empty() || cursor.len() > 2048)
        {
            return Err(invalid("invalid lyrics line limit or cursor"));
        }
        let track = self.lyrics_target(grant, state, &input.target).await?;
        let lyrics = self.lyrics.as_ref().ok_or_else(lyrics_unavailable)?;
        let cancel = CancellationToken::new();
        let mut result = match input.source.unwrap_or(LyricsSourcePreference::Auto) {
            LyricsSourcePreference::Auto => self
                .core
                .lyrics_query_scoped(
                    LyricsQueryRequest {
                        track: match_metadata(track)?,
                        set_user_offset_ms: None,
                        reset_user_offset: false,
                    },
                    Some(state),
                )
                .await
                .map_err(map_query_error)?,
            LyricsSourcePreference::Lrclib => lyrics
                .read_lrclib(match_metadata(track)?, cancel)
                .await
                .map_err(map_query_error)?,
            LyricsSourcePreference::YouTubeMusic => {
                let epoch = state.snapshot.account_epoch.as_ref().ok_or_else(|| {
                    ControlError::new(
                        ErrorCode::AuthRequired,
                        RetryDisposition::ReauthRequired,
                        "sign in to YouTube Music for official lyrics",
                    )
                })?;
                let official = self
                    .core
                    .official_lyrics_checked(&track.video_id, &cancel, Some(epoch))
                    .await
                    .map_err(map_query_error)?;
                LyricsResult {
                    video_id: track.video_id.clone(),
                    kind: if official.is_some() {
                        LyricsKind::Plain
                    } else {
                        LyricsKind::NotFound
                    },
                    source: "youtube-music".into(),
                    language: None,
                    match_basis: official
                        .as_ref()
                        .map(|value| format!("youtube-music:videoId; {}", value.source)),
                    fetched_at_ms: self.core.clock().wall_now_ms(),
                    file_offset_ms: 0,
                    user_offset_ms: 0,
                    synced_lines: Vec::new(),
                    plain_text: official.map(|value| value.text),
                    error: None,
                }
            }
        };
        if result.video_id != track.video_id {
            return Err(unavailable(
                "the lyrics provider returned a different video",
            ));
        }
        result.user_offset_ms = lyrics
            .user_offset(track.video_id.clone())
            .await
            .map_err(map_query_error)?;
        let after = self.authorized_state(grant, false).await?;
        if after.snapshot.account_epoch != state.snapshot.account_epoch
            || after.snapshot.player.bridge_epoch != state.snapshot.player.bridge_epoch
            || after.snapshot.player.track_instance_id != state.snapshot.player.track_instance_id
            || after
                .snapshot
                .player
                .current_track
                .as_ref()
                .map(|track| &track.video_id)
                != Some(&track.video_id)
        {
            return Err(stale("the lyrics target changed during the read"));
        }
        lyrics_page(result, cursor, usize::from(limit))
    }

    async fn lyrics_target<'a>(
        &self,
        grant: &ReadGrant,
        state: &'a ControlRead,
        target: &TrackTarget,
    ) -> Result<&'a Track, ControlError> {
        let current = state.snapshot.player.current_track.as_ref();
        match target {
            TrackTarget::Video { video_id } => current
                .filter(|track| &track.video_id == video_id)
                .ok_or_else(|| {
                    ControlError::new(
                        ErrorCode::CapabilityUnavailable,
                        RetryDisposition::NotRetryable,
                        "trusted lyrics metadata is available only for the current video",
                    )
                }),
            TrackTarget::Context { context_token } => {
                let binding = self.resolve_context(grant, context_token).await?;
                let video_id = binding
                    .video_id
                    .ok_or_else(|| invalid("the context has no track"))?;
                if binding.core_epoch != state.snapshot.core_epoch
                    || binding.account_epoch != state.snapshot.account_epoch
                    || binding.bridge_epoch != state.snapshot.player.bridge_epoch
                    || binding.track_instance_id != state.snapshot.player.track_instance_id
                {
                    return Err(stale(
                        "the lyric context is no longer the current playback instance",
                    ));
                }
                current
                    .filter(|track| track.video_id == video_id)
                    .ok_or_else(|| stale("the lyric context is no longer the current video"))
            }
        }
    }
}

impl MusicOperation {
    pub(super) async fn set_lyrics_offset(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::SetLyricsOffset { target, offset } = self.input.action.clone() else {
            return Err(invalid("expected a lyrics offset change"));
        };
        if let LyricsOffsetAction::Set { offset_ms } = offset
            && !(-MAX_OFFSET_MS..=MAX_OFFSET_MS).contains(&offset_ms)
        {
            return Err(invalid(
                "lyrics offset must be between -10000 and 10000 milliseconds",
            ));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        if self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch {
            return Err(stale("the lyrics offset account epoch does not match"));
        }
        let lyrics = self.service.lyrics.clone().ok_or_else(lyrics_unavailable)?;
        let video_id = self
            .service
            .lyrics_target(&self.grant, &self.state, &target)
            .await?
            .video_id
            .clone();

        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        self.apply(submitted).await?;

        let before_write = async {
            let grant = self.authority.authorize(GrantAction::Preferences).await?;
            let state = self.service.authorized_state(&self.grant, false).await?;
            if state.snapshot.account_epoch != self.state.snapshot.account_epoch
                || self
                    .input
                    .preconditions
                    .expected_bridge_epoch
                    .as_ref()
                    .is_some_and(|epoch| Some(epoch) != state.snapshot.player.bridge_epoch.as_ref())
            {
                return Err(stale("the session changed before saving the lyrics offset"));
            }
            self.service
                .lyrics_target(&self.grant, &state, &target)
                .await?;
            if grant.deadline_wall_ms <= self.service.core.clock().wall_now_ms() {
                return Err(permission(
                    "the grant expired before saving the lyrics offset",
                ));
            }
            Ok(())
        };
        if let Err(error) = before_write.await {
            return Ok(Transition::Rejected(error));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        match lyrics.change_offset(video_id.clone(), offset).await {
            Ok(offset_ms) => {
                // The desktop lyrics view refetches the offset of this video on the notice.
                if let Some(epoch) = self.state.snapshot.account_epoch.clone() {
                    let _ = self
                        .service
                        .core
                        .invalidate_control_resource(
                            epoch,
                            "lyricsOffset".into(),
                            video_id.as_str().into(),
                        )
                        .await;
                }
                self.current.resource = Some(OperationResource::LyricsOffset {
                    video_id,
                    offset_ms,
                });
                Ok(Transition::Verified(VerificationLevel::Persisted))
            }
            Err(error) => Ok(Transition::Rejected(ControlError::new(
                map_query_error(error).code,
                RetryDisposition::NotRetryable,
                "the lyrics offset could not be saved",
            ))),
        }
    }
}

fn lyrics_unavailable() -> ControlError {
    ControlError::new(
        ErrorCode::CapabilityUnavailable,
        RetryDisposition::NotRetryable,
        "lyrics service is unavailable",
    )
}

fn match_metadata(track: &Track) -> Result<LyricsMatchMetadata, ControlError> {
    let artist = track
        .artists
        .first()
        .filter(|artist| !artist.name.trim().is_empty())
        .ok_or_else(lyrics_unavailable)?;
    if track.title.trim().is_empty() {
        return Err(lyrics_unavailable());
    }
    Ok(LyricsMatchMetadata {
        video_id: track.video_id.clone(),
        title: track.title.clone(),
        artist: artist.name.clone(),
        album: track.album.as_ref().map(|album| album.title.clone()),
        duration_ms: track.duration_ms,
        version_tag: track.version_tag.clone(),
    })
}

fn lyrics_page(
    result: LyricsResult,
    cursor: Option<&str>,
    limit: usize,
) -> Result<LyricsPage, ControlError> {
    // Bind continuation to what was read. A refreshed lyric or another track cannot silently
    // reuse a line index from a previous response.
    let line_identity = serde_json::to_vec(&(
        &result.video_id,
        &result.kind,
        &result.source,
        &result.file_offset_ms,
        &result.synced_lines,
        &result.plain_text,
    ))
    .map_err(|_| unavailable("the lyrics page could not be encoded"))?;
    let revision = hex::encode(Sha256::digest(line_identity));
    let start = match cursor {
        Some(cursor) => cursor
            .split_once(':')
            .filter(|(value, _)| *value == revision)
            .and_then(|(_, index)| index.parse::<usize>().ok())
            .filter(|index| *index > 0)
            .ok_or_else(|| stale("the lyrics cursor no longer addresses these lines"))?,
        None => 0,
    };
    let mut page = LyricsPage {
        video_id: result.video_id,
        kind: result.kind,
        source: result.source,
        language: result.language,
        match_basis: result.match_basis,
        fetched_at_ms: result.fetched_at_ms,
        file_offset_ms: result.file_offset_ms,
        user_offset_ms: result.user_offset_ms,
        lines: Vec::new(),
        plain_text: result.plain_text.as_ref().map(|_| String::new()),
        next_cursor: None,
        complete: true,
    };
    let base = json_size(&page)? + CURSOR_OVERHEAD;
    let mut remaining = MAX_CONTENT_BYTES
        .checked_sub(base)
        .ok_or_else(|| unavailable("the lyrics metadata exceeds the response budget"))?;
    let mut next = start;
    let total = if let (Some(text), Some(plain_page)) =
        (result.plain_text.as_deref(), &mut page.plain_text)
    {
        for line in text.split('\n').skip(start).take(limit) {
            let separator = if next > start { 2 } else { 0 }; // JSON encodes newline as \\n.
            let bytes = json_size(line)? - 2 + separator; // The page already includes the enclosing quotes.
            if bytes > remaining {
                break;
            }
            if next > start {
                plain_page.push('\n');
            }
            plain_page.push_str(line);
            remaining -= bytes;
            next += 1;
        }
        text.split('\n').count()
    } else {
        let total = result.synced_lines.len();
        for line in result.synced_lines.into_iter().skip(start).take(limit) {
            let bytes = json_size(&line)? + usize::from(next > start);
            if bytes > remaining {
                break;
            }
            page.lines.push(line);
            remaining -= bytes;
            next += 1;
        }
        total
    };
    if start > 0 && start >= total {
        return Err(stale("the lyrics cursor is outside the lyrics"));
    }
    if next < total {
        if next == start {
            return Err(unavailable("a lyrics line exceeds the response budget"));
        }
        page.next_cursor = Some(format!("{revision}:{next}"));
        page.complete = false;
    }
    Ok(page)
}

fn json_size<T: serde::Serialize + ?Sized>(value: &T) -> Result<usize, ControlError> {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .map_err(|_| unavailable("the lyrics page could not be encoded"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_protocol::LyricsLine;

    fn sample() -> LyricsResult {
        LyricsResult {
            video_id: VideoId::new("fixture-song").unwrap(),
            kind: LyricsKind::Synced,
            source: "lrclib".into(),
            language: None,
            match_basis: None,
            fetched_at_ms: 0,
            file_offset_ms: 10,
            user_offset_ms: -100,
            synced_lines: vec![],
            plain_text: None,
            error: None,
        }
    }

    #[test]
    fn serialized_synced_pages_fit_budget_and_continue_at_the_next_line() {
        let mut result = sample();
        result.synced_lines = (0..200)
            .map(|i| LyricsLine {
                time_ms: i * 100,
                // A JSON-escaped quote, a backslash and a 3-byte CJK character inflate the size.
                text: "\"\\歌".repeat(300),
            })
            .collect();
        let first = lyrics_page(result.clone(), None, 200).unwrap();
        assert!(json_size(&first).unwrap() <= MAX_CONTENT_BYTES);
        let next = first.next_cursor.as_deref();
        assert!(!first.lines.is_empty());
        assert!(!first.complete);
        let second = lyrics_page(result.clone(), next, 200).unwrap();
        assert_eq!(second.lines[0], result.synced_lines[first.lines.len()]);
        assert_eq!(second.user_offset_ms, -100);
        result.synced_lines[0].text.push('x');
        assert_eq!(
            lyrics_page(result, next, 200).unwrap_err().code,
            ErrorCode::StaleContext
        );
    }

    #[test]
    fn plain_pages_preserve_indentation_blanks_and_source() {
        let mut result = sample();
        result.kind = LyricsKind::Plain;
        result.source = "youtube-music".into();
        result.plain_text = Some("  first\n\nlast".into());
        let first = lyrics_page(result.clone(), None, 2).unwrap();
        assert_eq!(first.plain_text.as_deref(), Some("  first\n"));
        assert_eq!(first.source, "youtube-music");
        assert!(first.lines.is_empty());
        let second = lyrics_page(result, first.next_cursor.as_deref(), 2).unwrap();
        assert_eq!(second.plain_text.as_deref(), Some("last"));
        assert!(second.complete);
    }
}
