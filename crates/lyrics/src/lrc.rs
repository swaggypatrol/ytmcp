use ytm_protocol::LyricsLine;

/// Metadata tags optionally extracted from LRC header lines (`[ti:...]`, `[ar:...]`, etc.).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LrcMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub language: Option<String>,
    pub author: Option<String>,
    pub by: Option<String>,
}

/// Result of parsing raw lyrics text (LRC or plain text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedLyrics {
    Synced {
        lines: Vec<LyricsLine>,
        file_offset_ms: i64,
        metadata: LrcMetadata,
    },
    Plain {
        text: String,
    },
    Empty,
}

/// Parses raw lyrics content into synced LRC cues, plain text fallback, or empty.
///
/// Handles:
/// - UTF-8 BOM stripping (`\u{FEFF}`)
/// - `\r\n` and `\n` line endings
/// - `[mm:ss]`, `[mm:ss.x]`, `[mm:ss.xx]`, `[mm:ss.xxx]` timestamps
/// - Multiple timestamps per line (`[00:12.00][01:05.50]Chorus`)
/// - `[offset:±ms]` header tag (stored in `file_offset_ms`, separate from `LyricsLine::time_ms`;
///   positive means lyrics appear earlier, see [`effective_line_time_ms`])
/// - Duplicate timestamps (preserved in original relative order)
/// - Out-of-order timestamps (stably sorted by `time_ms`)
/// - Blank and malformed lines tolerated without failing valid lines
/// - Plain text preserved verbatim (never interprets, strips, or escapes HTML)
pub fn parse_lyrics(raw: &str) -> ParsedLyrics {
    let normalized = normalize_input(raw);
    if normalized.trim().is_empty() {
        return ParsedLyrics::Empty;
    }

    let mut lines: Vec<LyricsLine> = Vec::new();
    let mut file_offset_ms: i64 = 0;
    let mut metadata = LrcMetadata::default();
    let mut non_tag_plain_lines: Vec<&str> = Vec::new();

    for raw_line in normalized.split('\n') {
        let trimmed_start = raw_line.trim_start_matches([' ', '\t']);
        if trimmed_start.is_empty() {
            continue;
        }

        // Try parsing one or more leading LRC timestamps: `[mm:ss(.fff)]...`
        let (timestamps, rest) = parse_leading_timestamps(trimmed_start);
        if !timestamps.is_empty() {
            let text = rest.trim().to_string();
            for time_ms in timestamps {
                lines.push(LyricsLine {
                    time_ms,
                    text: text.clone(),
                });
            }
            continue;
        }

        // Check for known LRC header/metadata tags like `[offset:+500]`, `[ti:Title]`, etc.
        if parse_header_tag(trimmed_start, &mut file_offset_ms, &mut metadata) {
            continue;
        }

        // Not a valid timestamped line or header tag: keep track in case the whole file
        // has no valid synced timestamps (fallback to plain text).
        non_tag_plain_lines.push(raw_line);
    }

    let has_non_empty_synced = lines.iter().any(|line| !line.text.is_empty());
    if !lines.is_empty() && (has_non_empty_synced || non_tag_plain_lines.is_empty()) {
        lines.sort_by_key(|line| line.time_ms);
        return ParsedLyrics::Synced {
            lines,
            file_offset_ms,
            metadata,
        };
    }

    // Fallback to plain text preserved verbatim (BOM stripped, CRLF normalized).
    ParsedLyrics::Plain { text: normalized }
}

/// Computes the effective display timestamp for a line, saturating at `0`:
/// `line_time_ms - file_offset_ms + user_offset_ms`.
///
/// - `file_offset_ms` follows the LRC convention: a positive `[offset:]` makes lyrics appear
///   earlier.
/// - `user_offset_ms` is a delay: a positive value makes lyrics appear later (the UI's
///   "0.5 s later" button adds, "0.5 s earlier" subtracts).
///
/// `ui/src/lib/lyrics.ts` `effectiveLineTimeMs` must stay identical.
pub fn effective_line_time_ms(line_time_ms: u64, file_offset_ms: i64, user_offset_ms: i64) -> u64 {
    let base = i128::from(line_time_ms);
    let shifted = base
        .saturating_sub(i128::from(file_offset_ms))
        .saturating_add(i128::from(user_offset_ms));
    if shifted <= 0 {
        0
    } else {
        u64::try_from(shifted).unwrap_or(u64::MAX)
    }
}

fn normalize_input(raw: &str) -> String {
    let without_bom = raw.trim_start_matches('\u{FEFF}');
    without_bom.replace("\r\n", "\n").replace('\r', "\n")
}

fn parse_leading_timestamps(mut input: &str) -> (Vec<u64>, &str) {
    let mut timestamps = Vec::new();
    while let Some(after_open) = input.strip_prefix('[') {
        let Some(close_idx) = after_open.find(']') else {
            break;
        };
        let tag_body = &after_open[..close_idx];
        if let Some(ms) = parse_timestamp_body(tag_body) {
            timestamps.push(ms);
            input = &after_open[close_idx + 1..];
        } else {
            break;
        }
    }
    (timestamps, input)
}

fn parse_timestamp_body(body: &str) -> Option<u64> {
    let body = body.trim();
    let (min_str, sec_part) = body.split_once(':')?;
    if min_str.is_empty() || !min_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minutes: u64 = min_str.parse().ok()?;

    let (sec_str, frac_ms) = match sec_part.split_once('.') {
        Some((s, f)) => {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let frac = parse_fraction_ms(f)?;
            (s, frac)
        }
        None => (sec_part, 0),
    };

    if sec_str.len() != 2 || !sec_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds: u64 = sec_str.parse().ok()?;
    if seconds >= 60 {
        return None;
    }

    minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(1_000)?
        .checked_add(frac_ms)
}

fn parse_fraction_ms(frac: &str) -> Option<u64> {
    let bytes = frac.as_bytes();
    let ms = match bytes.len() {
        1 => u64::from(bytes[0] - b'0') * 100,
        2 => (u64::from(bytes[0] - b'0') * 10 + u64::from(bytes[1] - b'0')) * 10,
        3 => {
            u64::from(bytes[0] - b'0') * 100
                + u64::from(bytes[1] - b'0') * 10
                + u64::from(bytes[2] - b'0')
        }
        _ => {
            // Truncate sub-millisecond digits to milliseconds.
            u64::from(bytes[0] - b'0') * 100
                + u64::from(bytes[1] - b'0') * 10
                + u64::from(bytes[2] - b'0')
        }
    };
    Some(ms)
}

fn parse_header_tag(line: &str, file_offset_ms: &mut i64, metadata: &mut LrcMetadata) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return false;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let Some((key, val)) = inner.split_once(':') else {
        return false;
    };
    let key_lower = key.trim().to_ascii_lowercase();
    let val_trimmed = val.trim();

    match key_lower.as_str() {
        "offset" => {
            if let Ok(parsed) = val_trimmed.parse::<i64>() {
                *file_offset_ms = parsed;
            }
            true
        }
        "ti" => {
            if !val_trimmed.is_empty() {
                metadata.title = Some(val_trimmed.to_string());
            }
            true
        }
        "ar" => {
            if !val_trimmed.is_empty() {
                metadata.artist = Some(val_trimmed.to_string());
            }
            true
        }
        "al" => {
            if !val_trimmed.is_empty() {
                metadata.album = Some(val_trimmed.to_string());
            }
            true
        }
        "au" => {
            if !val_trimmed.is_empty() {
                metadata.author = Some(val_trimmed.to_string());
            }
            true
        }
        "by" => {
            if !val_trimmed.is_empty() {
                metadata.by = Some(val_trimmed.to_string());
            }
            true
        }
        "la" | "lang" => {
            if !val_trimmed.is_empty() {
                metadata.language = Some(val_trimmed.to_string());
            }
            true
        }
        "re" | "ve" | "length" => true,
        _ => false,
    }
}
