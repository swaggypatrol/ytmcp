use std::collections::BTreeSet;
use std::fmt;
use ytm_protocol::LyricsMatchMetadata;

/// Default maximum allowed duration difference (8 seconds) when both durations are known.
pub const DEFAULT_MAX_DURATION_DELTA_MS: u64 = 8_000;

/// Default minimum confidence score required to accept a lyrics candidate.
pub const DEFAULT_MIN_CONFIDENCE_SCORE: f64 = 0.65;

/// Normalized version markers extracted from track metadata and candidate strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VersionMarker {
    Live,
    Acoustic,
    Remaster,
    Remix,
    Instrumental,
    Karaoke,
    RadioEdit,
    Demo,
    SpedUp,
    Slowed,
}

impl VersionMarker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Acoustic => "acoustic",
            Self::Remaster => "remaster",
            Self::Remix => "remix",
            Self::Instrumental => "instrumental",
            Self::Karaoke => "karaoke",
            Self::RadioEdit => "radio edit",
            Self::Demo => "demo",
            Self::SpedUp => "sped up",
            Self::Slowed => "slowed",
        }
    }
}

impl fmt::Display for VersionMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Candidate lyrics payload evaluated by the matcher (typically from LRCLIB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsCandidate {
    pub id: Option<u64>,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: Option<String>,
    pub duration_ms: Option<u64>,
    pub instrumental: bool,
    pub synced_lyrics: Option<String>,
    pub plain_lyrics: Option<String>,
}

/// Explicit reason a candidate failed acceptance gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchRejectionReason {
    VersionMismatch {
        requested: Vec<VersionMarker>,
        candidate: Vec<VersionMarker>,
    },
    DurationMismatch {
        delta_ms: u64,
        max_delta_ms: u64,
    },
    LowConfidence {
        score_pct: u8,
        min_score_pct: u8,
    },
    EmptyContent,
}

impl fmt::Display for MatchRejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionMismatch {
                requested,
                candidate,
            } => write!(
                f,
                "version_mismatch (requested={}, candidate={})",
                format_markers(requested),
                format_markers(candidate)
            ),
            Self::DurationMismatch {
                delta_ms,
                max_delta_ms,
            } => write!(
                f,
                "duration_mismatch (delta_ms={}, max_ms={})",
                delta_ms, max_delta_ms
            ),
            Self::LowConfidence {
                score_pct,
                min_score_pct,
            } => write!(
                f,
                "low_confidence (score={}%, min={}%)",
                score_pct, min_score_pct
            ),
            Self::EmptyContent => f.write_str("empty_content"),
        }
    }
}

/// Result of scoring a single candidate against requested track metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchEvaluation {
    pub accepted: bool,
    pub score: f64,
    pub duration_delta_ms: Option<u64>,
    pub requested_markers: Vec<VersionMarker>,
    pub candidate_markers: Vec<VersionMarker>,
    pub rejection: Option<MatchRejectionReason>,
    pub match_basis: String,
}

/// Configuration thresholds for version-aware lyrics matching.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchConfig {
    pub max_duration_delta_ms: u64,
    pub min_confidence_score: f64,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            max_duration_delta_ms: DEFAULT_MAX_DURATION_DELTA_MS,
            min_confidence_score: DEFAULT_MIN_CONFIDENCE_SCORE,
        }
    }
}

/// Extracts all normalized version markers from requested track metadata.
pub fn extract_requested_markers(track: &LyricsMatchMetadata) -> Vec<VersionMarker> {
    let mut set = BTreeSet::new();
    if let Some(tag) = &track.version_tag {
        extract_markers_from_text(tag, true, true, &mut set);
    }
    extract_markers_from_text(&track.title, false, true, &mut set);
    if let Some(album) = &track.album {
        // Only treat live/acoustic/etc. from album when in clear version context;
        // do not treat album-level "Remastered" as conflicting for a studio track title.
        extract_markers_from_text(album, false, false, &mut set);
    }
    set.into_iter().collect()
}

/// Extracts all normalized version markers from a candidate record.
pub fn extract_candidate_markers(candidate: &LyricsCandidate) -> Vec<VersionMarker> {
    let mut set = BTreeSet::new();
    extract_markers_from_text(&candidate.track_name, false, true, &mut set);
    if let Some(album) = &candidate.album_name {
        extract_markers_from_text(album, false, false, &mut set);
    }
    if candidate.instrumental && set.is_empty() {
        // Note: instrumental flag on a studio song's instrumental track is handled via
        // LyricsKind::Instrumental; we don't mark a normal song as version=instrumental
        // solely from the boolean if the requested track isn't tagged instrumental, unless
        // the title itself says instrumental.
    }
    set.into_iter().collect()
}

/// Scores a candidate against requested track metadata with strict version & duration gates.
pub fn evaluate_candidate(
    track: &LyricsMatchMetadata,
    candidate: &LyricsCandidate,
    source_label: &str,
    config: MatchConfig,
) -> MatchEvaluation {
    let requested_markers = extract_requested_markers(track);
    let candidate_markers = extract_candidate_markers(candidate);

    let duration_delta_ms = match (track.duration_ms, candidate.duration_ms) {
        (Some(req_d), Some(cand_d)) => Some(req_d.abs_diff(cand_d)),
        _ => None,
    };

    let has_content = candidate.instrumental
        || candidate
            .synced_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        || candidate
            .plain_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());

    if !has_content {
        let reason = MatchRejectionReason::EmptyContent;
        return MatchEvaluation {
            accepted: false,
            score: 0.0,
            duration_delta_ms,
            requested_markers,
            candidate_markers,
            match_basis: format!("{source_label}: rejected: {reason}"),
            rejection: Some(reason),
        };
    }

    if requested_markers != candidate_markers {
        let reason = MatchRejectionReason::VersionMismatch {
            requested: requested_markers.clone(),
            candidate: candidate_markers.clone(),
        };
        return MatchEvaluation {
            accepted: false,
            score: 0.0,
            duration_delta_ms,
            requested_markers,
            candidate_markers,
            match_basis: format!("{source_label}: rejected: {reason}"),
            rejection: Some(reason),
        };
    }

    if let Some(delta) = duration_delta_ms
        && delta > config.max_duration_delta_ms
    {
        let reason = MatchRejectionReason::DurationMismatch {
            delta_ms: delta,
            max_delta_ms: config.max_duration_delta_ms,
        };
        return MatchEvaluation {
            accepted: false,
            score: 0.0,
            duration_delta_ms,
            requested_markers,
            candidate_markers,
            match_basis: format!("{source_label}: rejected: {reason}"),
            rejection: Some(reason),
        };
    }

    let title_sim = title_similarity(&track.title, &candidate.track_name);
    let artist_sim = artist_similarity(&track.artist, &candidate.artist_name);
    let album_sim = match (&track.album, &candidate.album_name) {
        (Some(a), Some(b)) if !a.trim().is_empty() && !b.trim().is_empty() => {
            token_similarity(&normalize_base_text(a), &normalize_base_text(b))
        }
        _ => 0.5,
    };
    let duration_sim = match duration_delta_ms {
        Some(delta) => {
            let max_d = config.max_duration_delta_ms.max(1) as f64;
            (1.0 - (delta as f64 / max_d)).clamp(0.0, 1.0)
        }
        None => 0.5,
    };

    let score = (title_sim * 0.50 + artist_sim * 0.35 + duration_sim * 0.10 + album_sim * 0.05)
        .clamp(0.0, 1.0);

    // Prevent false positives where title matches a completely different artist or vice versa.
    let hard_text_pass = title_sim >= 0.45 && artist_sim >= 0.35;
    if !hard_text_pass || score < config.min_confidence_score {
        let score_pct = (score * 100.0).round().clamp(0.0, 100.0) as u8;
        let min_score_pct = (config.min_confidence_score * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        let reason = MatchRejectionReason::LowConfidence {
            score_pct,
            min_score_pct,
        };
        return MatchEvaluation {
            accepted: false,
            score,
            duration_delta_ms,
            requested_markers,
            candidate_markers,
            match_basis: format!("{source_label}: rejected: {reason}"),
            rejection: Some(reason),
        };
    }

    let version_desc = format_markers(&requested_markers);
    let basis = match duration_delta_ms {
        Some(delta) => format!(
            "{source_label}: accepted (score={score:.2}, duration_delta_ms={delta}, version={version_desc})"
        ),
        None => format!(
            "{source_label}: accepted (score={score:.2}, duration_delta_ms=unknown, version={version_desc})"
        ),
    };

    MatchEvaluation {
        accepted: true,
        score,
        duration_delta_ms,
        requested_markers,
        candidate_markers,
        rejection: None,
        match_basis: basis,
    }
}

/// Selects the highest-scoring accepted candidate from a list (e.g. `/api/search` results).
/// Returns either `Ok((index, evaluation))` or `Err(summary_match_basis)` explaining rejections.
pub fn select_best_candidate(
    track: &LyricsMatchMetadata,
    candidates: &[LyricsCandidate],
    source_label: &str,
    config: MatchConfig,
) -> Result<(usize, MatchEvaluation), String> {
    if candidates.is_empty() {
        return Err(format!("{source_label}: no candidates returned"));
    }

    let mut best: Option<(usize, MatchEvaluation, u8)> = None;
    let mut rejection_notes: Vec<String> = Vec::new();

    for (idx, candidate) in candidates.iter().enumerate() {
        let eval = evaluate_candidate(track, candidate, source_label, config);
        if eval.accepted {
            let content_rank = if candidate
                .synced_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            {
                2
            } else if candidate
                .plain_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            {
                1
            } else {
                0
            };

            let replace = match &best {
                None => true,
                Some((_, best_eval, best_rank)) => {
                    // Prefer substantially higher score, or synced over plain when scores are close.
                    let score_diff = eval.score - best_eval.score;
                    score_diff > 0.05 || (score_diff.abs() <= 0.05 && content_rank > *best_rank)
                }
            };
            if replace {
                best = Some((idx, eval, content_rank));
            }
        } else if let Some(rej) = &eval.rejection {
            rejection_notes.push(rej.to_string());
        }
    }

    match best {
        Some((idx, eval, _)) => Ok((idx, eval)),
        None => {
            let first_reason = rejection_notes
                .first()
                .map(String::as_str)
                .unwrap_or("no suitable candidate");
            Err(format!(
                "{source_label}: rejected {} candidate(s): {}",
                candidates.len(),
                first_reason
            ))
        }
    }
}

fn format_markers(markers: &[VersionMarker]) -> String {
    if markers.is_empty() {
        "studio".to_string()
    } else {
        markers
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join("+")
    }
}

fn extract_markers_from_text(
    text: &str,
    is_explicit_version_tag: bool,
    include_remaster: bool,
    out: &mut BTreeSet<VersionMarker>,
) {
    let lower = text.to_ascii_lowercase();
    let padded = normalize_marker_text(&lower);

    // Live detection: avoid false positives on verb uses like "Live Forever" or "Live and Let Die"
    // unless inside an explicit version_tag or standard version delimiter context.
    if (is_explicit_version_tag && contains_phrase(&padded, "live"))
        || has_contextual_live_marker(&lower)
    {
        out.insert(VersionMarker::Live);
    }

    if contains_phrase(&padded, "acoustic") || contains_phrase(&padded, "unplugged") {
        out.insert(VersionMarker::Acoustic);
    }
    if include_remaster
        && (contains_phrase(&padded, "remaster") || contains_phrase(&padded, "remastered"))
    {
        out.insert(VersionMarker::Remaster);
    }
    if contains_phrase(&padded, "remix") {
        out.insert(VersionMarker::Remix);
    }
    if contains_phrase(&padded, "instrumental") {
        out.insert(VersionMarker::Instrumental);
    }
    if contains_phrase(&padded, "karaoke") {
        out.insert(VersionMarker::Karaoke);
    }
    if contains_phrase(&padded, "radio edit") || contains_phrase(&padded, "radio version") {
        out.insert(VersionMarker::RadioEdit);
    }
    if contains_phrase(&padded, "demo") {
        out.insert(VersionMarker::Demo);
    }
    if contains_phrase(&padded, "sped up") || contains_phrase(&padded, "speed up") {
        out.insert(VersionMarker::SpedUp);
    }
    if contains_phrase(&padded, "slowed") {
        out.insert(VersionMarker::Slowed);
    }
}

fn has_contextual_live_marker(lower: &str) -> bool {
    // Check parenthetical / bracketed groups containing the word "live"
    for (open, close) in [('(', ')'), ('[', ']')] {
        let mut rest = lower;
        while let Some(start) = rest.find(open) {
            let after_open = &rest[start + 1..];
            if let Some(end) = after_open.find(close) {
                let inner = normalize_marker_text(&after_open[..end]);
                if contains_phrase(&inner, "live") {
                    return true;
                }
                rest = &after_open[end + 1..];
            } else {
                break;
            }
        }
    }

    // Check dash-separated suffix: "... - live", "... - live at ...", "... — live"
    for sep in [" - ", " – ", " — "] {
        if let Some((_, suffix)) = lower.split_once(sep) {
            let norm_suffix = normalize_marker_text(suffix);
            if contains_phrase(&norm_suffix, "live") {
                return true;
            }
        }
    }

    // Explicit phrases in titles/albums
    let padded = normalize_marker_text(lower);
    [
        "live at",
        "live from",
        "live in",
        "live on",
        "live version",
        "live ver",
        "live session",
        "live recording",
        "live performance",
    ]
    .iter()
    .any(|phrase| contains_phrase(&padded, phrase))
}

fn normalize_marker_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(' ');
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else {
            out.push(' ');
        }
    }
    out.push(' ');
    out
}

fn contains_phrase(padded_normalized: &str, phrase: &str) -> bool {
    let needle = format!(" {phrase} ");
    padded_normalized.contains(&needle)
}

fn title_similarity(req_title: &str, cand_title: &str) -> f64 {
    let full_req = normalize_base_text(req_title);
    let full_cand = normalize_base_text(cand_title);
    let base_req = normalize_base_text(&strip_parentheticals(req_title));
    let base_cand = normalize_base_text(&strip_parentheticals(cand_title));

    let full_sim = token_similarity(&full_req, &full_cand);
    let base_sim = token_similarity(&base_req, &base_cand);
    full_sim.max(base_sim)
}

fn artist_similarity(req_artist: &str, cand_artist: &str) -> f64 {
    let norm_req = normalize_artist(req_artist);
    let norm_cand = normalize_artist(cand_artist);
    if norm_req == norm_cand && !norm_req.is_empty() {
        return 1.0;
    }
    token_similarity(&norm_req, &norm_cand)
}

fn normalize_artist(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let stripped = lower
        .split(" feat.")
        .next()
        .unwrap_or(&lower)
        .split(" ft.")
        .next()
        .unwrap_or(&lower)
        .split(" featuring ")
        .next()
        .unwrap_or(&lower);
    normalize_base_text(stripped)
}

fn strip_parentheticals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth_paren = 0usize;
    let mut depth_bracket = 0usize;
    for ch in s.chars() {
        match ch {
            '(' => depth_paren += 1,
            ')' if depth_paren > 0 => depth_paren -= 1,
            '[' => depth_bracket += 1,
            ']' if depth_bracket > 0 => depth_bracket -= 1,
            _ if depth_paren == 0 && depth_bracket == 0 => out.push(ch),
            _ => {}
        }
    }
    // Also strip trailing " - Remastered..." / " - Live..." suffix if present
    for sep in [" - ", " – ", " — "] {
        if let Some((head, _)) = out.split_once(sep) {
            return head.to_string();
        }
    }
    out
}

fn normalize_base_text(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.join(" ")
}

fn token_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let set_a: BTreeSet<&str> = a.split_whitespace().collect();
    let set_b: BTreeSet<&str> = b.split_whitespace().collect();
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        return 0.0;
    }
    let jaccard = intersection as f64 / union as f64;
    let min_len = set_a.len().min(set_b.len()) as f64;
    let containment = if min_len > 0.0 {
        intersection as f64 / min_len
    } else {
        0.0
    };
    // Blend Jaccard and containment lightly so "Artist" vs "Artist & Guest" scores reasonably
    // while unrelated strings stay low.
    (jaccard * 0.75 + containment * 0.25).clamp(0.0, 1.0)
}
