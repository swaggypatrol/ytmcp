//! The listener profile: a compact picture of the signed-in listener's own music, read from their
//! liked songs, their playlists, the plays this player recorded and the stored taste notes.
//!
//! It describes the music only. Nothing about the interface language, time zone, account locale
//! or the conversation goes into it; the script figures come from titles and artist names.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use ytm_control_api::{
    ControlError, ControlRequest, ControlResponse, DjTrack, ErrorCode, LibraryInput, LibraryKind,
    LibraryPage, ListenerProfile, ListenerProfileInput, PageInput, ProfileArtist, ProfilePlaylist,
    ProfileSample, RetryDisposition, Script, ScriptShare, Track,
};
use ytm_protocol::{AccountEpoch, AccountScope};

use super::{ControlRead, ControlService, ReadGrant, track_projection, unavailable};

/// Liked songs read for one profile, from the start of the list.
const LIKED_SAMPLE: usize = 200;
/// Local plays read for one profile, newest first.
const HISTORY_SAMPLE: usize = 200;
const PLAYLIST_LIMIT: usize = 30;
const TOP_ARTISTS: usize = 15;
const RECENT_TRACKS: usize = 12;
/// How long a catalog part is reused for the same account session.
const PROFILE_TTL_MS: u64 = 15 * 60 * 1000;
/// How long one catalog part may take, well inside the control server's request timeout, so a
/// slow catalog costs only its own part.
const CATALOG_PART_DEADLINE: Duration = Duration::from_secs(10);

/// The catalog parts of the profile (liked songs and playlists), each kept for the account scope
/// and epoch it was read in, so a logout or another account never sees them. Local history and
/// taste notes are cheap local reads and are read on every call, so local edits show at once.
pub(super) struct ProfileCache {
    liked: Kept<(Vec<Track>, bool)>,
    playlists: Kept<(Vec<ProfilePlaylist>, bool)>,
    deadline: Duration,
}

impl Default for ProfileCache {
    fn default() -> Self {
        Self {
            liked: Kept::default(),
            playlists: Kept::default(),
            deadline: CATALOG_PART_DEADLINE,
        }
    }
}

/// One kept catalog part.
struct Kept<T>(Mutex<Option<KeptPart<T>>>);

impl<T> Default for Kept<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

struct KeptPart<T> {
    scope: AccountScope,
    epoch: AccountEpoch,
    expires_at_ms: u64,
    value: T,
}

impl<T: Clone> Kept<T> {
    /// The kept value for this account session; a value of another session is dropped.
    fn get(&self, scope: &AccountScope, epoch: &AccountEpoch, now_ms: u64) -> Option<T> {
        let mut slot = self.0.lock().ok()?;
        match slot.as_ref() {
            Some(kept)
                if &kept.scope == scope && &kept.epoch == epoch && kept.expires_at_ms > now_ms =>
            {
                Some(kept.value.clone())
            }
            Some(_) => {
                *slot = None;
                None
            }
            None => None,
        }
    }

    fn set(&self, part: Option<KeptPart<T>>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = part;
        }
    }

    /// The kept value unless `refresh` is set, otherwise `read` within `deadline`. A refresh
    /// drops the kept value first, so a failed refresh never leaves an older copy to be served.
    /// Without a sign-in epoch nothing is read or kept.
    async fn read_through(
        &self,
        session: Option<(&AccountScope, &AccountEpoch)>,
        refresh: bool,
        now_ms: u64,
        deadline: Duration,
        read: impl Future<Output = Result<T, ControlError>>,
    ) -> Result<T, ControlError> {
        let Some((scope, epoch)) = session else {
            return Err(ControlError::new(
                ErrorCode::AuthRequired,
                RetryDisposition::ReauthRequired,
                "sign in to YouTube Music again",
            ));
        };
        if refresh {
            self.set(None);
        } else if let Some(value) = self.get(scope, epoch, now_ms) {
            return Ok(value);
        }
        let value = tokio::time::timeout(deadline, read)
            .await
            .map_err(|_| unavailable("the catalog took too long for the listener profile"))??;
        self.set(Some(KeptPart {
            scope: scope.clone(),
            epoch: epoch.clone(),
            expires_at_ms: now_ms.saturating_add(PROFILE_TTL_MS),
            value: value.clone(),
        }));
        Ok(value)
    }
}

impl ControlService {
    /// Sets how long one catalog part of the listener profile may take; the default is ten
    /// seconds. Meant for tests and embedders with their own request timeout.
    pub fn with_profile_part_deadline(mut self, deadline: Duration) -> Self {
        self.profiles.deadline = deadline;
        self
    }

    /// Serves `GetListenerProfile`. Liked songs and playlists come from the kept copy for this
    /// account session unless `refresh` is set; local history and taste notes are read every
    /// time. A part that cannot be read is named in `unavailable`. While the sign-in has
    /// expired, the catalog parts are unavailable and the local parts are still served.
    pub(super) async fn read_listener_profile(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        input: ListenerProfileInput,
    ) -> Result<ListenerProfile, ControlError> {
        let auth = &state.snapshot.auth_state;
        let Some(scope) = auth.account_scope.clone() else {
            return Err(ControlError::new(
                ErrorCode::AuthRequired,
                RetryDisposition::ReauthRequired,
                "sign in to YouTube Music first",
            ));
        };
        let session = auth.account_epoch.as_ref().map(|epoch| (&scope, epoch));
        let now = self.core.clock().now_ms();
        let cache = &self.profiles;
        let (liked, history, playlists, taste_notes) = tokio::join!(
            cache.liked.read_through(
                session,
                input.refresh,
                now,
                cache.deadline,
                self.sample_liked(grant, state),
            ),
            self.sample_history(&scope),
            cache.playlists.read_through(
                session,
                input.refresh,
                now,
                cache.deadline,
                self.first_playlists(grant, state),
            ),
            self.count_taste_notes(&scope),
        );
        // An account change during the reads makes the whole profile stale, not a missing part.
        self.check_read_context(grant, state).await?;
        let mut unavailable = Vec::new();
        for (part, failed) in [
            ("liked", liked.is_err()),
            ("history", history.is_err()),
            ("playlists", playlists.is_err()),
            ("tasteNotes", taste_notes.is_err()),
        ] {
            if failed {
                unavailable.push(part.to_owned());
            }
        }
        let liked = liked.ok();
        let history = history.ok();
        let (scripts, scripts_sampled) = script_shares(
            liked
                .iter()
                .flat_map(|(tracks, _)| tracks)
                .chain(history.iter().flat_map(|(tracks, _)| tracks)),
        );
        let (playlists, more_playlists) = playlists.unwrap_or_default();
        Ok(ListenerProfile {
            generated_at_ms: self.core.clock().wall_now_ms(),
            liked: liked.map_or_else(empty_sample, |(tracks, complete)| {
                let total = complete.then(|| count(tracks.len()));
                sample(&tracks, total)
            }),
            history: history
                .map_or_else(empty_sample, |(tracks, total)| sample(&tracks, Some(total))),
            playlists,
            more_playlists,
            scripts,
            scripts_sampled,
            taste_notes: taste_notes.unwrap_or(0),
            unavailable,
        })
    }

    /// The first liked songs, through the same read as the liked library. Returns the tracks and
    /// whether they are the whole list.
    async fn sample_liked(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
    ) -> Result<(Vec<Track>, bool), ControlError> {
        let mut tracks = Vec::new();
        let mut cursor = None;
        loop {
            let limit =
                (LIKED_SAMPLE - tracks.len()).min(usize::from(ytm_control_api::MAX_PAGE_SIZE));
            let response = self
                .read_catalog_view(
                    grant,
                    state,
                    ControlRequest::GetLibrary(LibraryInput {
                        kind: LibraryKind::Liked,
                        page: Some(PageInput {
                            cursor: cursor.take(),
                            limit: Some(limit as u16),
                        }),
                    }),
                )
                .await?;
            let ControlResponse::Library(LibraryPage::Liked(page)) = response else {
                return Err(unavailable("the catalog returned an unexpected liked page"));
            };
            let read_any = !page.items.is_empty();
            tracks.extend(page.items);
            match page.next_cursor {
                None => return Ok((tracks, true)),
                Some(next) if read_any && tracks.len() < LIKED_SAMPLE => cursor = Some(next),
                Some(_) => return Ok((tracks, false)),
            }
        }
    }

    /// The newest plays recorded by this player and how many it holds for the account.
    async fn sample_history(
        &self,
        scope: &AccountScope,
    ) -> Result<(Vec<Track>, u32), ControlError> {
        let store = self
            .history
            .as_deref()
            .ok_or_else(|| unavailable("local playback history is unavailable"))?;
        let history = store.snapshot(scope.clone()).await?;
        let total = count(history.records.len());
        let tracks = history
            .records
            .iter()
            .take(HISTORY_SAMPLE)
            .map(|record| track_projection(&record.track))
            .collect();
        Ok((tracks, total))
    }

    /// The first library playlists, and whether more exist.
    async fn first_playlists(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
    ) -> Result<(Vec<ProfilePlaylist>, bool), ControlError> {
        let response = self
            .read_catalog_view(
                grant,
                state,
                ControlRequest::GetLibrary(LibraryInput {
                    kind: LibraryKind::Playlists,
                    page: Some(PageInput {
                        cursor: None,
                        limit: Some(PLAYLIST_LIMIT as u16),
                    }),
                }),
            )
            .await?;
        let ControlResponse::Library(LibraryPage::Playlists(page)) = response else {
            return Err(unavailable(
                "the catalog returned an unexpected playlist page",
            ));
        };
        let more = page.next_cursor.is_some() || page.items.len() > PLAYLIST_LIMIT;
        let playlists = page
            .items
            .into_iter()
            .take(PLAYLIST_LIMIT)
            .map(|playlist| ProfilePlaylist {
                playlist_id: playlist.playlist_id,
                title: playlist.title,
                track_count: playlist.track_count,
            })
            .collect();
        Ok((playlists, more))
    }

    async fn count_taste_notes(&self, scope: &AccountScope) -> Result<u32, ControlError> {
        self.dj_store()?.count_preferences(scope.clone()).await
    }
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn empty_sample() -> ProfileSample {
    ProfileSample {
        total: None,
        sampled: 0,
        top_artists: Vec::new(),
        recent: Vec::new(),
    }
}

/// One source in brief. `tracks` are newest first; a track played twice is one recent entry,
/// but every play counts towards its artists.
fn sample(tracks: &[Track], total: Option<u32>) -> ProfileSample {
    let mut seen = HashSet::new();
    ProfileSample {
        total,
        sampled: count(tracks.len()),
        top_artists: top_artists(tracks),
        recent: tracks
            .iter()
            .filter(|track| seen.insert(&track.video_id))
            .take(RECENT_TRACKS)
            .map(DjTrack::from)
            .collect(),
    }
}

/// Artists by the number of sampled tracks naming them, most first; equal counts keep the order
/// in which the artists first appear. Names are matched without regard to case.
fn top_artists(tracks: &[Track]) -> Vec<ProfileArtist> {
    let mut artists: Vec<ProfileArtist> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for track in tracks {
        let mut in_track = HashSet::new();
        for artist in &track.artists {
            let name = artist.name.trim();
            let key = name.to_lowercase();
            if name.is_empty() || !in_track.insert(key.clone()) {
                continue;
            }
            match index.get(&key) {
                Some(&at) => artists[at].tracks += 1,
                None => {
                    index.insert(key, artists.len());
                    artists.push(ProfileArtist {
                        name: name.to_owned(),
                        tracks: 1,
                    });
                }
            }
        }
    }
    artists.sort_by_key(|artist| std::cmp::Reverse(artist.tracks));
    artists.truncate(TOP_ARTISTS);
    artists
}

/// Scripts in a fixed order, used to break ties and to order equal shares.
const SCRIPTS: [Script; 11] = [
    Script::Latin,
    Script::Han,
    Script::Kana,
    Script::Hangul,
    Script::Cyrillic,
    Script::Arabic,
    Script::Devanagari,
    Script::Thai,
    Script::Hebrew,
    Script::Greek,
    Script::Other,
];

fn slot(script: Script) -> usize {
    SCRIPTS
        .iter()
        .position(|s| *s == script)
        .unwrap_or(SCRIPTS.len() - 1)
}

/// Script shares over distinct tracks (by video ID), largest first, with whole percentages that
/// add up to 100. Returns the shares and the number of tracks counted.
fn script_shares<'a>(tracks: impl Iterator<Item = &'a Track>) -> (Vec<ScriptShare>, u32) {
    let mut seen = HashSet::new();
    let mut counts = [0u32; SCRIPTS.len()];
    for track in tracks.filter(|track| seen.insert(&track.video_id)) {
        let texts = std::iter::once(track.title.as_str())
            .chain(track.artists.iter().map(|artist| artist.name.as_str()));
        counts[slot(track_script(texts))] += 1;
    }
    let total: u32 = counts.iter().sum();
    if total == 0 {
        return (Vec::new(), 0);
    }
    // Largest remainder: floor every share, then give the missing points to the largest
    // remainders, so the percentages add up to exactly 100.
    let mut shares: Vec<(usize, u32, u64, u64)> = counts
        .iter()
        .enumerate()
        .filter(|(_, tracks)| **tracks > 0)
        .map(|(at, tracks)| {
            let scaled = u64::from(*tracks) * 100;
            (
                at,
                *tracks,
                scaled / u64::from(total),
                scaled % u64::from(total),
            )
        })
        .collect();
    let missing = 100 - shares.iter().map(|share| share.2).sum::<u64>();
    let mut by_remainder: Vec<usize> = (0..shares.len()).collect();
    by_remainder.sort_by(|a, b| {
        let (a, b) = (&shares[*a], &shares[*b]);
        b.3.cmp(&a.3).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0))
    });
    for at in by_remainder.into_iter().take(missing as usize) {
        shares[at].2 += 1;
    }
    shares.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let shares = shares
        .into_iter()
        .map(|(at, tracks, percent, _)| ScriptShare {
            script: SCRIPTS[at],
            tracks,
            percent: percent as u8,
        })
        .collect();
    (shares, total)
}

/// The script of most of the letters in a track's title and artist names. Han and kana letters
/// are counted together, as Japanese mixes them: when together they are the most, the track is
/// Kana if any kana appear and Han otherwise. Ties go to the earlier script in `SCRIPTS`. Digits,
/// punctuation, symbols and spaces are not letters; a track without letters is Other.
pub(super) fn track_script<'a>(texts: impl IntoIterator<Item = &'a str>) -> Script {
    let mut counts = [0u32; SCRIPTS.len()];
    for letter in texts.into_iter().flat_map(str::chars) {
        if let Some(script) = letter_script(letter) {
            counts[slot(script)] += 1;
        }
    }
    let (han, kana) = (counts[slot(Script::Han)], counts[slot(Script::Kana)]);
    counts[slot(Script::Han)] = han + kana;
    counts[slot(Script::Kana)] = 0;
    let (best, letters) =
        counts
            .iter()
            .enumerate()
            .fold((SCRIPTS.len() - 1, 0), |best, (at, letters)| {
                if *letters > best.1 {
                    (at, *letters)
                } else {
                    best
                }
            });
    match SCRIPTS[best] {
        _ if letters == 0 => Script::Other,
        Script::Han if kana > 0 => Script::Kana,
        script => script,
    }
}

/// The script of one letter, or `None` when the character is not a letter.
fn letter_script(c: char) -> Option<Script> {
    if !c.is_alphabetic() {
        return None;
    }
    let script = match u32::from(c) {
        // Combining diacritics belong to the letter before them.
        0x0300..=0x036F => return None,
        0x0041..=0x024F | 0x0250..=0x02AF | 0x1D00..=0x1DBF | 0x1E00..=0x1EFF => Script::Latin,
        0x2C60..=0x2C7F | 0xA720..=0xA7FF | 0xAB30..=0xAB6F | 0xFF21..=0xFF5A => Script::Latin,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => Script::Cyrillic,
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::Hebrew,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x0870..=0x08FF | 0xFB50..=0xFDFF => Script::Arabic,
        0xFE70..=0xFEFF => Script::Arabic,
        0x0900..=0x097F | 0xA8E0..=0xA8FF => Script::Devanagari,
        0x0E00..=0x0E7F => Script::Thai,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7FF => Script::Hangul,
        0xFFA0..=0xFFDC => Script::Hangul,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F | 0x1B000..=0x1B16F => Script::Kana,
        0x3005..=0x3007 | 0x3021..=0x3029 | 0x3400..=0x4DBF | 0x4E00..=0x9FFF => Script::Han,
        0xF900..=0xFAFF | 0x20000..=0x323AF => Script::Han,
        _ => Script::Other,
    };
    Some(script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_control_api::{Artist, Rating, VideoId};
    use ytm_protocol::TrackAvailability;

    fn script(title: &str, artists: &[&str]) -> Script {
        track_script(std::iter::once(title).chain(artists.iter().copied()))
    }

    fn track(id: &str, title: &str, artists: &[&str]) -> Track {
        Track {
            video_id: VideoId::new(id).unwrap(),
            title: title.into(),
            artists: artists
                .iter()
                .map(|name| Artist {
                    id: None,
                    name: (*name).into(),
                })
                .collect(),
            album: None,
            duration_ms: None,
            availability: TrackAvailability::Available,
            version_tag: None,
            rating: Rating::Unknown,
        }
    }

    #[test]
    fn a_track_takes_the_script_of_most_of_its_letters() {
        assert_eq!(script("Bohemian Rhapsody", &["Queen"]), Script::Latin);
        assert_eq!(
            script("Ça plane pour moi", &["Plastic Bertrand"]),
            Script::Latin
        );
        // Han words with a longer Latin artist name count as Latin: letters decide, not words.
        assert_eq!(script("晴天", &["Jay Chou"]), Script::Latin);
        assert_eq!(script("晴天 (Live)", &["周杰伦"]), Script::Han);
        assert_eq!(script("봄날", &["방탄소년단"]), Script::Hangul);
        assert_eq!(script("Dynamite", &["방탄소년단"]), Script::Latin);
        assert_eq!(script("Кино", &["Группа крови"]), Script::Cyrillic);
        assert_eq!(script("تملي معاك", &["عمرو دياب"]), Script::Arabic);
    }

    #[test]
    fn kana_with_kanji_is_kana_and_kanji_alone_is_han() {
        // Two kanji and three kana: pooled, then kana because some appear.
        assert_eq!(script("夜に駆ける", &[]), Script::Kana);
        // Six kanji and two kana still count as kana, as the kanji are pooled with it.
        assert_eq!(script("君の名は", &["中島美嘉"]), Script::Kana);
        assert_eq!(script("千本桜", &["初音"]), Script::Han);
        assert_eq!(script("ﾊﾟﾌﾟﾘｶ", &[]), Script::Kana);
    }

    #[test]
    fn digits_punctuation_and_spaces_are_not_letters() {
        assert_eq!(script("1999", &["2 + 2 = 5"]), Script::Other);
        assert_eq!(script("", &[]), Script::Other);
        assert_eq!(script("4'33\"", &["♪ ★ —"]), Script::Other);
        assert_eq!(script("1989 (Taylor's Version)", &["!!!"]), Script::Latin);
        // A tie goes to the earlier script.
        assert_eq!(script("ab", &["가나"]), Script::Latin);
    }

    #[test]
    fn shares_count_each_track_once_and_add_up_to_one_hundred() {
        let tracks = [
            track("a", "One", &[]),
            track("b", "Two", &[]),
            track("c", "가나", &[]),
            track("a", "One", &[]),
            track("d", "Кино", &[]),
            track("e", "...", &[]),
        ];
        let (shares, sampled) = script_shares(tracks.iter());
        assert_eq!(sampled, 5);
        let got: Vec<_> = shares
            .iter()
            .map(|share| (share.script, share.tracks, share.percent))
            .collect();
        assert_eq!(
            got,
            [
                (Script::Latin, 2, 40),
                (Script::Hangul, 1, 20),
                (Script::Cyrillic, 1, 20),
                (Script::Other, 1, 20),
            ]
        );
        let thirds = [
            track("a", "One", &[]),
            track("b", "가나", &[]),
            track("c", "Кино", &[]),
        ];
        let (shares, _) = script_shares(thirds.iter());
        assert_eq!(
            shares
                .iter()
                .map(|share| u32::from(share.percent))
                .sum::<u32>(),
            100
        );
        assert_eq!(shares[0].percent, 34);
    }

    #[test]
    fn top_artists_count_tracks_ignore_case_and_keep_first_seen_order() {
        let tracks = [
            track("a", "x", &["Beta", "Alpha"]),
            track("b", "x", &["alpha", "ALPHA"]),
            track("c", "x", &["Beta"]),
            track("d", "x", &["Gamma", " "]),
        ];
        let got: Vec<_> = top_artists(&tracks)
            .into_iter()
            .map(|artist| (artist.name, artist.tracks))
            .collect();
        assert_eq!(
            got,
            [("Beta".into(), 2), ("Alpha".into(), 2), ("Gamma".into(), 1)]
        );
    }
}
