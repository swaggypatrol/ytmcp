mod client;
mod outcome;
mod register;

use std::{path::PathBuf, sync::Arc};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use ytm_control_api::{
    CapabilityArea, ContextInput, ControlError, ControlRequest, ControlResponse, ErrorCode,
    OperationInput, PageInput, PlaylistInput, Preconditions, RecommendationsInput,
    RetryDisposition, SearchInput, UpcomingStatus, WriteAction, WriteInput,
};

use crate::client::ControlClient;
use ytm_control_api::{PlaylistId, QueueMode, Rating, TrackTarget, VideoId};

const USAGE: &str = "Usage: ytmcp --credential-file ABSOLUTE_PATH [--socket auto] [--profile common|development] [--allow-background-start]";

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CapabilitiesInput {
    area: Option<CapabilityArea>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    #[schemars(length(min = 1, max = 256))]
    query: String,
    #[schemars(length(min = 1, max = 2048))]
    cursor: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u16>,
    mode: Option<SearchMode>,
    kind: Option<SearchKind>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum SearchKind {
    Song,
    Album,
    Artist,
    Playlist,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum SearchMode {
    Results,
    Suggestions,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlaybackArguments {
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    preconditions: Preconditions,
    action: PlaybackAction,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum PlaybackAction {
    Resume,
    Pause,
    Stop,
    Next,
    Previous,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnqueueArguments {
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    preconditions: Preconditions,
    context_token: String,
    mode: QueueMode,
    #[schemars(length(min = 1, max = 50))]
    video_ids: Vec<VideoId>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InterruptArguments {
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    preconditions: Preconditions,
    context_token: String,
    #[schemars(length(min = 1, max = 50))]
    video_ids: Vec<VideoId>,
    resume_policy: ytm_control_api::InterruptResumePolicy,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreatePlaylistArguments {
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    preconditions: Preconditions,
    #[schemars(length(min = 1, max = 150))]
    title: String,
    #[schemars(length(max = 5000))]
    description: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppendArguments {
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    preconditions: Preconditions,
    playlist_id: PlaylistId,
    created_by_operation_id: Option<String>,
    #[schemars(length(min = 1, max = 50))]
    video_ids: Vec<VideoId>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RatingArguments {
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    preconditions: Preconditions,
    target: TrackTarget,
    rating: Rating,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccountHandoffArguments {
    operation_id: String,
    preconditions: Preconditions,
    goal: ytm_control_api::AccountHandoffGoal,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DjFeedbackArguments {
    operation_id: String,
    preconditions: Preconditions,
    change: ytm_control_api::DjFeedbackChange,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListeningIntentArguments {
    operation_id: String,
    preconditions: Preconditions,
    change: ytm_control_api::ListeningIntentChange,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsArguments {
    /// `device` (default) returns the settings and revision; `djPreferences` pages the recorded
    /// DJ preferences, newest first.
    section: Option<SettingsSection>,
    page: Option<PageInput>,
}

#[derive(Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
enum SettingsSection {
    Device,
    DjPreferences,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateSettingsArguments {
    operation_id: String,
    preconditions: Preconditions,
    expected_settings_revision: u64,
    patch: ytm_control_api::SettingsPatch,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LyricsOffsetArguments {
    operation_id: String,
    preconditions: Preconditions,
    target: TrackTarget,
    offset: ytm_control_api::LyricsOffsetAction,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditPlaylistArguments {
    operation_id: String,
    preconditions: Preconditions,
    playlist_id: PlaylistId,
    edit: PlaylistEdit,
}

#[derive(Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum PlaylistEdit {
    Metadata {
        title: Option<String>,
        description: Option<String>,
    },
    Privacy {
        privacy: ytm_control_api::PlaylistPrivacy,
    },
    Remove {
        #[schemars(length(min = 1, max = 50))]
        entry_ids: Vec<String>,
    },
    Move {
        entry_id: String,
        before_entry_id: Option<String>,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeekArguments {
    operation_id: String,
    preconditions: Preconditions,
    context_token: String,
    position_ms: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VolumeArguments {
    operation_id: String,
    preconditions: Preconditions,
    #[schemars(range(min = 0, max = 1))]
    volume: f64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlayContextArguments {
    operation_id: String,
    preconditions: Preconditions,
    target: ytm_control_api::PlayContextTarget,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HomeArguments {
    kind: Option<ytm_control_api::DiscoveryKind>,
    page: Option<PageInput>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u16>,
    #[schemars(length(min = 64, max = 64))]
    category_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrackArguments {
    target: TrackTarget,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlaybackModeArguments {
    operation_id: String,
    preconditions: Preconditions,
    change: PlaybackMode,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlayerArguments {
    include: Option<ytm_control_api::PlayerInclude>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtistSubscriptionArguments {
    operation_id: String,
    preconditions: Preconditions,
    artist_id: ytm_control_api::ArtistId,
    subscribed: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryMembershipArguments {
    operation_id: String,
    preconditions: Preconditions,
    target: ytm_control_api::LibraryMembershipTarget,
    saved: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum PlaybackMode {
    Shuffle {
        enabled: bool,
    },
    Repeat {
        mode: ytm_control_api::RepeatMode,
    },
    Mute {
        muted: bool,
    },
    Autoplay {
        enabled: bool,
    },
    /// A `filterId` from `queue.filters` in ytm_get_context.
    UpNextFilter {
        #[schemars(length(min = 1, max = 64))]
        filter_id: String,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueueEditArguments {
    operation_id: String,
    preconditions: Preconditions,
    context_token: String,
    change: QueueEdit,
}

#[derive(Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum QueueEdit {
    Select {
        entry_id: ytm_control_api::QueueEntryId,
    },
    Remove {
        entry_id: ytm_control_api::QueueEntryId,
    },
    Move {
        entry_id: ytm_control_api::QueueEntryId,
        before_entry_id: Option<ytm_control_api::QueueEntryId>,
    },
    ClearUpcoming,
    ReplaceUpcoming {
        #[schemars(length(min = 1, max = 50))]
        video_ids: Vec<VideoId>,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryEditArguments {
    operation_id: String,
    preconditions: Preconditions,
    source: WritableHistorySource,
    action: ytm_control_api::LocalHistoryEdit,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum WritableHistorySource {
    Local,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlaylistMetadataArguments {
    operation_id: String,
    preconditions: Preconditions,
    playlist_id: ytm_control_api::PlaylistId,
    patch: PlaylistMetadataPatch,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaylistMetadataPatch {
    #[schemars(length(min = 1, max = 150))]
    title: Option<String>,
    #[schemars(length(max = 5000))]
    description: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitPromptArguments {
    /// The listener talked to you since your previous call.
    #[serde(default)]
    user_spoke: bool,
    /// You said something to the listener since your previous call.
    #[serde(default)]
    spoke: bool,
    /// How often check-ins should come; it stays set until changed.
    chattiness: Option<ytm_control_api::DjChattiness>,
    /// Defaults to 1200. Each track's ending prompt normally arrives well before that.
    #[schemars(range(min = 1, max = 1800))]
    max_wait_seconds: Option<u32>,
    /// The coreEpoch and seq of the last prompt you received. A prompt handed to a wait you
    /// cancelled is then offered again; skip repeats by (coreEpoch, seq).
    last_received: Option<ytm_control_api::DjCueAck>,
}

#[derive(Clone, Copy)]
enum MusicTool {
    WaitPrompt,
    AccountHandoff,
    EditQueue,
    Interrupt,
    DjFeedback,
    SetListeningIntent,
    Player,
    Track,
    PlaybackMode,
    ArtistSubscription,
    LibraryMembership,
    Album,
    Artist,
    History,
    ManageHistory,
    Home,
    Library,
    ListenerProfile,
    ExtendedMedia,
    EditPlaylist,
    UpdatePlaylist,
    Lyrics,
    LyricsOffset,
    Settings,
    UpdateSettings,
    PlayContext,
    Seek,
    Volume,
    Append,
    Rating,
    Playback,
    CreatePlaylist,
    Enqueue,
    Account,
    Capabilities,
    Context,
    Playlist,
    ShareLink,
    Queue,
    Recommendations,
    Search,
}

impl MusicTool {
    const ORDERED: [Self; 41] = [
        Self::AccountHandoff,
        Self::Append,
        Self::Playback,
        Self::CreatePlaylist,
        Self::EditPlaylist,
        Self::EditQueue,
        Self::Enqueue,
        Self::Account,
        Self::Album,
        Self::Artist,
        Self::Capabilities,
        Self::Context,
        Self::ExtendedMedia,
        Self::History,
        Self::Home,
        Self::Library,
        Self::ListenerProfile,
        Self::Lyrics,
        Self::Player,
        Self::Playlist,
        Self::Queue,
        Self::Recommendations,
        Self::Settings,
        Self::ShareLink,
        Self::Track,
        Self::Interrupt,
        Self::ManageHistory,
        Self::PlayContext,
        Self::Search,
        Self::Seek,
        Self::ArtistSubscription,
        Self::DjFeedback,
        Self::LibraryMembership,
        Self::SetListeningIntent,
        Self::LyricsOffset,
        Self::PlaybackMode,
        Self::Rating,
        Self::Volume,
        Self::UpdatePlaylist,
        Self::UpdateSettings,
        Self::WaitPrompt,
    ];

    fn read_only(self) -> bool {
        matches!(
            self,
            Self::Account
                | Self::Capabilities
                | Self::Context
                | Self::Lyrics
                | Self::Player
                | Self::Track
                | Self::Album
                | Self::Artist
                | Self::History
                | Self::Home
                | Self::Library
                | Self::ListenerProfile
                | Self::ExtendedMedia
                | Self::Playlist
                | Self::ShareLink
                | Self::Queue
                | Self::Recommendations
                | Self::Settings
                | Self::Search
                | Self::WaitPrompt
        )
    }

    fn writes(self) -> bool {
        self.accepts("operationId")
    }

    /// Whether the tool's input has this top-level field.
    fn accepts(self, field: &str) -> bool {
        let definition = self.definition();
        definition
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|properties| properties.contains_key(field))
    }

    fn open_world(self) -> bool {
        !matches!(
            self,
            Self::Account
                | Self::Capabilities
                | Self::Context
                | Self::DjFeedback
                | Self::SetListeningIntent
                | Self::AccountHandoff
                | Self::ManageHistory
                | Self::Player
                | Self::Queue
                | Self::Settings
                | Self::UpdateSettings
                | Self::LyricsOffset
                | Self::WaitPrompt
        )
    }

    fn name(self) -> &'static str {
        match self {
            Self::WaitPrompt => "ytm_wait_dj_prompt",
            Self::Player => "ytm_get_player",
            Self::Track => "ytm_get_track",
            Self::PlaybackMode => "ytm_set_playback_mode",
            Self::ArtistSubscription => "ytm_set_artist_subscription",
            Self::LibraryMembership => "ytm_set_library_membership",
            Self::Album => "ytm_get_album",
            Self::Artist => "ytm_get_artist",
            Self::History => "ytm_get_history",
            Self::ManageHistory => "ytm_manage_history",
            Self::ExtendedMedia => "ytm_get_extended_media",
            Self::Home => "ytm_get_home",
            Self::Library => "ytm_get_library",
            Self::ListenerProfile => "ytm_get_listener_profile",
            Self::EditQueue => "ytm_edit_queue",
            Self::Interrupt => "ytm_interrupt_with",
            Self::AccountHandoff => "ytm_account_handoff",
            Self::DjFeedback => "ytm_set_dj_feedback",
            Self::SetListeningIntent => "ytm_set_listening_intent",
            Self::EditPlaylist => "ytm_edit_playlist_entries",
            Self::UpdatePlaylist => "ytm_update_playlist",
            Self::Lyrics => "ytm_get_lyrics",
            Self::LyricsOffset => "ytm_set_lyrics_offset",
            Self::Settings => "ytm_get_settings",
            Self::UpdateSettings => "ytm_update_settings",
            Self::PlayContext => "ytm_play_context",
            Self::Seek => "ytm_seek",
            Self::Volume => "ytm_set_volume",
            Self::Rating => "ytm_set_rating",
            Self::Append => "ytm_add_playlist_entries",
            Self::CreatePlaylist => "ytm_create_playlist",
            Self::Enqueue => "ytm_enqueue",
            Self::Playback => "ytm_control_playback",
            Self::Playlist => "ytm_get_playlist",
            Self::ShareLink => "ytm_get_share_link",
            Self::Recommendations => "ytm_get_recommendations",
            Self::Account => "ytm_get_account",
            Self::Capabilities => "ytm_get_capabilities",
            Self::Context => "ytm_get_context",
            Self::Queue => "ytm_get_queue",
            Self::Search => "ytm_search",
        }
    }

    fn definition(self) -> Tool {
        let (description, input) = match self {
            Self::WaitPrompt => (
                "Be the listener's DJ: call this in a loop. It returns when there is something to react to: a prompt shortly before each track ends, an occasional moment to check in with the listener, or a signal soon after they steer the music by hand (skipping several tracks, picking something, going back to hear a track again, changing the Up Next filter), with what is playing, what is queued, how recent tracks ended (finished or skipped), what the listener did by hand since your previous prompt, how long they have been quiet and their recorded tastes. What they do with the controls always wins: never undo it. Before you first choose music in a session, read ytm_get_listener_profile and choose from the scripts and styles of their own music, not by your training or the language of the conversation. Act on it as you see fit with the other music tools, or do nothing, then call again. After you speak to the listener, call this again right away in the same turn instead of stopping to wait for an answer: their answer arrives with a later result. Report userSpoke and spoke from your conversation so check-ins come at sensible times; for five minutes after either, each call returns within about a minute so you hear the listener. Without a prompt it returns empty after maxWaitSeconds. A client that cancels waits passes lastReceived so a prompt lost with a cancelled wait comes again.",
                schema::<WaitPromptArguments>(),
            ),
            Self::Player => (
                "Read observed and pending playback, page audio observations, or both.",
                schema::<PlayerArguments>(),
            ),
            Self::Track => (
                "Read the song bound to an observed context and query the rating when available.",
                schema::<TrackArguments>(),
            ),
            Self::PlaybackMode => (
                "Set one repeat, shuffle, mute or queue autoplay mode, or pick an Up Next filter \
                 (upNextFilter with a filterId from ytm_get_context queue.filters, such as \
                 Familiar or Discover). A filter changes only YouTube Music's own suggestions \
                 after the queued tracks; the current track keeps playing and queued tracks stay. \
                 After a refusal, read the context again for current filters.",
                schema::<PlaybackModeArguments>(),
            ),
            Self::ArtistSubscription => (
                "Set subscription to an exact YouTube Music artist channel and verify it with a fresh read.",
                schema::<ArtistSubscriptionArguments>(),
            ),
            Self::LibraryMembership => (
                "Save or remove a song, an exact album release or a supported third-party playlist in the current account, with independent readback.",
                schema::<LibraryMembershipArguments>(),
            ),
            Self::Album => (
                "Read album tracks in provider order. Set includeMembership to read the saved state for an exact edition.",
                schema::<ytm_control_api::AlbumInput>(),
            ),
            Self::Artist => (
                "Read the artist overview and its available highlights.",
                schema::<ytm_control_api::ArtistInput>(),
            ),
            Self::History => (
                "Read local or YouTube history, or both separately without merging repeated plays. Local history includes observed timestamps and a revision.",
                schema::<ytm_control_api::HistoryInput>(),
            ),
            Self::ManageHistory => (
                "Remove exact local history occurrences or clear the local history in a time range using its current revision. YouTube history is unaffected.",
                schema::<HistoryEditArguments>(),
            ),
            Self::Home => (
                "Read official discovery. To browse playlists for a mood or genre, supply a categoryId from the current session's moodsAndGenres page.",
                schema::<HomeArguments>(),
            ),
            Self::Library => (
                "Read saved songs, albums, artists, subscriptions, playlists, liked music, uploads or saved podcasts.",
                schema::<ytm_control_api::LibraryInput>(),
            ),
            Self::ListenerProfile => (
                "Read a compact picture of the listener's own music: how many liked songs and \
                 plays on this player it was built from, the artists that appear most in each, \
                 the newest tracks, their playlists, the writing scripts of the titles and artist \
                 names, and how many taste notes are stored. Read it before you first choose \
                 music for this listener in a session, and let it and what they do by hand guide \
                 the choice. The scripts describe what they listen to, not their language or \
                 country: choose within the scripts and styles their library shows and do not \
                 lean toward any language, country or scene because of your training or the \
                 language of the conversation. When it is still unclear what they want, ask \
                 them, or, within what this client may do, look further in the background with \
                 ytm_get_library and ytm_get_history. Liked songs and \
                 playlists are kept for 15 minutes; refresh reads them again. Parts that could \
                 not be read are listed in unavailable.",
                schema::<ytm_control_api::ListenerProfileInput>(),
            ),
            Self::ExtendedMedia => (
                "Browse uploaded music by artist or album and read podcast shows, channels, and episodes.",
                schema::<ytm_control_api::ExtendedMediaInput>(),
            ),
            Self::EditQueue => (
                "Select, move or remove a known occurrence, clear up to 50 upcoming entries, or replace them with tracks in input order, without affecting the active or earlier occurrences.",
                schema::<QueueEditArguments>(),
            ),
            Self::Interrupt => (
                "Play tracks now, interrupting the current one. resumeInterrupted replays the interrupted track from where it stopped, then the original queue; continueOriginalQueue returns to the original upcoming entries; stayOnInsertedContext starts a new context. Restoring policies need a queue whose end is loaded; the interrupted track then comes back on its own.",
                schema::<InterruptArguments>(),
            ),
            Self::EditPlaylist => (
                "Apply one editable playlist metadata, privacy, or occurrence change.",
                schema::<EditPlaylistArguments>(),
            ),
            Self::UpdatePlaylist => (
                "Update the title or description of an editable playlist and verify the exact fields by reading them back.",
                schema::<PlaylistMetadataArguments>(),
            ),
            Self::Lyrics => (
                "Read a bounded page of official or LRCLIB lyrics.",
                schema::<ytm_control_api::LyricsInput>(),
            ),
            Self::LyricsOffset => (
                "Persist a local lyric timing adjustment for this track.",
                schema::<LyricsOffsetArguments>(),
            ),
            Self::Settings => (
                "Read the current device settings and their revision, or page the recorded DJ preferences, newest first, with section djPreferences.",
                schema::<SettingsArguments>(),
            ),
            Self::AccountHandoff => (
                "Ask the user to sign in, renew or switch the YouTube Music account in the player. Nothing opens or takes focus; the request waits in the player's activity view until the change is observed or 30 minutes pass. A client registered for another account scope must be registered again afterwards.",
                schema::<AccountHandoffArguments>(),
            ),
            Self::DjFeedback => (
                "Remember or forget what the listener likes or wants to avoid: record prefer or avoid for a video or artist with an optional short note, or clear an entry by preferenceId. Entries are kept for the account until cleared and come with every DJ prompt. YouTube likes are unchanged.",
                schema::<DjFeedbackArguments>(),
            ),
            Self::SetListeningIntent => (
                "Set or clear a temporary listening intent (vocals, energy, activity, a short music brief, or dismissing an occasion by its id) for today, a 6-hour session, or until a time within 48 hours, when the listener shares intents with this client in Settings. Use basis 'userRequest' when the listener asked and 'inference' for your own guesses; an inference never replaces what the listener asked for. An inference must pass expectedMomentRevision from the moment you decided on; if it is stale, read the context again and decide again rather than retrying with the new revision. To clear one intent you read, pass its intentKey and intentId, so an intent that replaced it is kept.",
                schema::<ListeningIntentArguments>(),
            ),
            Self::UpdateSettings => (
                "Apply a narrowly scoped settings patch at a specific revision.",
                schema::<UpdateSettingsArguments>(),
            ),
            Self::PlayContext => (
                "Play a video, a captured track, a playlist or album from an exact position, or a captured queue occurrence.",
                schema::<PlayContextArguments>(),
            ),
            Self::Seek => (
                "Seek within the track instance bound to a captured context.",
                schema::<SeekArguments>(),
            ),
            Self::Volume => (
                "Set the application player volume in the 0–1 range.",
                schema::<VolumeArguments>(),
            ),
            Self::Rating => (
                "Set a song rating and request a new upstream readback.",
                schema::<RatingArguments>(),
            ),
            Self::Append => (
                "Append tracks in input order to a permitted playlist.",
                schema::<AppendArguments>(),
            ),
            Self::CreatePlaylist => (
                "Create a real private YouTube Music playlist.",
                schema::<CreatePlaylistArguments>(),
            ),
            Self::Enqueue => (
                "Insert tracks in input order after the current occurrence or at the verified end.",
                schema::<EnqueueArguments>(),
            ),
            Self::Playback => (
                "Resume, pause or stop playback, or skip to the next or previous track.",
                schema::<PlaybackArguments>(),
            ),
            Self::Playlist => (
                "Read an upstream playlist window and its occurrence IDs.",
                schema::<PlaylistInput>(),
            ),
            Self::ShareLink => (
                "Read a share link for an observed song or verified editable playlist, including known playlist visibility.",
                schema::<ytm_control_api::ShareInput>(),
            ),
            Self::Recommendations => (
                "Find real YouTube Music song radio candidates without changing playback.",
                schema::<RecommendationsInput>(),
            ),
            Self::Account => (
                "Read the current account and connection state.",
                schema::<NoInput>(),
            ),
            Self::Capabilities => (
                "Inspect which music capabilities are available.",
                schema::<CapabilitiesInput>(),
            ),
            Self::Context => (
                "Capture the current song, playback state and queue together.",
                schema::<ContextInput>(),
            ),
            Self::Queue => (
                "Read a page of the currently loaded queue in playback order.",
                schema::<PageInput>(),
            ),
            Self::Search => (
                "Search an individual entity kind: song, album, artist or playlist. Suggestions mode returns anonymous public predictions.",
                schema::<SearchArguments>(),
            ),
        };
        Tool::new(self.name(), description, optional_bookkeeping(input))
            .with_raw_output_schema(Arc::new(schema::<ControlResponse>()))
            .with_annotations(
                ToolAnnotations::new()
                    .read_only(self.read_only())
                    .open_world(self.open_world()),
            )
    }

    fn request(self, arguments: JsonObject) -> Result<ControlRequest, serde_json::Error> {
        let value = serde_json::Value::Object(arguments);
        Ok(match self {
            Self::WaitPrompt => {
                let input: WaitPromptArguments = serde_json::from_value(value)?;
                ControlRequest::NextDjCue(ytm_control_api::DjCueInput {
                    user_spoke: input.user_spoke,
                    spoke: input.spoke,
                    chattiness: input.chattiness,
                    last_received: input.last_received,
                })
            }
            Self::Player => {
                let input: PlayerArguments = serde_json::from_value(value)?;
                ControlRequest::GetPlayer {
                    include: input.include,
                }
            }
            Self::Track => {
                let input: TrackArguments = serde_json::from_value(value)?;
                ControlRequest::GetTrack {
                    target: input.target,
                }
            }
            Self::PlaybackMode => {
                let input: PlaybackModeArguments = serde_json::from_value(value)?;
                let action = match input.change {
                    PlaybackMode::Shuffle { enabled } => WriteAction::Shuffle { enabled },
                    PlaybackMode::Repeat { mode } => WriteAction::Repeat { mode },
                    PlaybackMode::Mute { muted } => WriteAction::Mute { muted },
                    PlaybackMode::Autoplay { enabled } => WriteAction::Autoplay { enabled },
                    PlaybackMode::UpNextFilter { filter_id } => {
                        WriteAction::SetQueueFilter { filter_id }
                    }
                };
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action,
                })
            }
            Self::ArtistSubscription => {
                let input: ArtistSubscriptionArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::SetArtistSubscription {
                        artist_id: input.artist_id,
                        subscribed: input.subscribed,
                    },
                })
            }
            Self::LibraryMembership => {
                let input: LibraryMembershipArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::SetLibraryMembership {
                        target: input.target,
                        saved: input.saved,
                    },
                })
            }
            Self::Album => ControlRequest::GetAlbum(serde_json::from_value(value)?),
            Self::Artist => ControlRequest::GetArtist(serde_json::from_value(value)?),
            Self::History => ControlRequest::GetHistory(serde_json::from_value(value)?),
            Self::ManageHistory => {
                let input: HistoryEditArguments = serde_json::from_value(value)?;
                let WritableHistorySource::Local = input.source;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::EditLocalHistory { edit: input.action },
                })
            }
            Self::Home => {
                let input: HomeArguments = serde_json::from_value(value)?;
                let page = input.page.or(input.limit.map(|limit| PageInput {
                    cursor: None,
                    limit: Some(limit),
                }));
                if let Some(category_id) = input.category_id {
                    if input
                        .kind
                        .is_some_and(|kind| kind != ytm_control_api::DiscoveryKind::MoodsAndGenres)
                    {
                        return Err(serde::de::Error::custom(
                            "categoryId requires moodsAndGenres",
                        ));
                    }
                    ControlRequest::GetMoodCategory(ytm_control_api::MoodCategoryInput {
                        category_id,
                        page,
                    })
                } else {
                    ControlRequest::GetDiscovery(ytm_control_api::DiscoveryInput {
                        kind: input.kind.unwrap_or(ytm_control_api::DiscoveryKind::Home),
                        page,
                    })
                }
            }

            Self::Library => ControlRequest::GetLibrary(serde_json::from_value(value)?),
            Self::ListenerProfile => {
                ControlRequest::GetListenerProfile(serde_json::from_value(value)?)
            }
            Self::ExtendedMedia => ControlRequest::GetExtendedMedia(serde_json::from_value(value)?),
            Self::EditQueue => {
                let input: QueueEditArguments = serde_json::from_value(value)?;
                let context_token = input.context_token;
                let action = match input.change {
                    QueueEdit::Select { entry_id } => WriteAction::SelectQueueEntry {
                        context_token,
                        entry_id,
                    },
                    QueueEdit::Remove { entry_id } => WriteAction::RemoveQueueEntry {
                        context_token,
                        entry_id,
                    },
                    QueueEdit::Move {
                        entry_id,
                        before_entry_id,
                    } => WriteAction::MoveQueueEntry {
                        context_token,
                        entry_id,
                        before_entry_id,
                    },
                    QueueEdit::ClearUpcoming => WriteAction::ClearUpcoming { context_token },
                    QueueEdit::ReplaceUpcoming { video_ids } => WriteAction::ReplaceUpcoming {
                        context_token,
                        video_ids,
                    },
                };
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action,
                })
            }
            Self::Interrupt => {
                let input: InterruptArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::InterruptWith {
                        context_token: input.context_token,
                        video_ids: input.video_ids,
                        resume_policy: input.resume_policy,
                    },
                })
            }
            Self::EditPlaylist => {
                let input: EditPlaylistArguments = serde_json::from_value(value)?;
                let playlist_id = input.playlist_id;
                let action = match input.edit {
                    PlaylistEdit::Metadata { title, description } => WriteAction::UpdatePlaylist {
                        playlist_id,
                        title,
                        description,
                    },
                    PlaylistEdit::Privacy { privacy } => WriteAction::SetPlaylistPrivacy {
                        playlist_id,
                        privacy,
                    },
                    PlaylistEdit::Remove { entry_ids } => WriteAction::RemovePlaylistEntries {
                        playlist_id,
                        entry_ids,
                    },
                    PlaylistEdit::Move {
                        entry_id,
                        before_entry_id,
                    } => WriteAction::MovePlaylistEntry {
                        playlist_id,
                        entry_id,
                        before_entry_id,
                    },
                };
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action,
                })
            }
            Self::UpdatePlaylist => {
                let input: PlaylistMetadataArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::UpdatePlaylist {
                        playlist_id: input.playlist_id,
                        title: input.patch.title,
                        description: input.patch.description,
                    },
                })
            }
            Self::Settings => {
                let input: SettingsArguments = serde_json::from_value(value)?;
                if input.section == Some(SettingsSection::DjPreferences) {
                    ControlRequest::GetDjPreferences(ytm_control_api::DjPreferencesInput {
                        page: input.page,
                    })
                } else if input.page.is_some() {
                    return Err(serde::de::Error::custom(
                        "page applies to the djPreferences section",
                    ));
                } else {
                    ControlRequest::GetSettings
                }
            }
            Self::AccountHandoff => {
                let input: AccountHandoffArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::AccountHandoff { goal: input.goal },
                })
            }
            Self::DjFeedback => {
                let input: DjFeedbackArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::SetDjFeedback {
                        change: input.change,
                    },
                })
            }
            Self::SetListeningIntent => {
                let input: ListeningIntentArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::SetListeningIntent {
                        change: input.change,
                    },
                })
            }
            Self::UpdateSettings => {
                let input: UpdateSettingsArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::UpdateSettings {
                        expected_settings_revision: input.expected_settings_revision,
                        patch: input.patch,
                    },
                })
            }
            Self::Lyrics => ControlRequest::GetLyrics(serde_json::from_value(value)?),
            Self::LyricsOffset => {
                let input: LyricsOffsetArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::SetLyricsOffset {
                        target: input.target,
                        offset: input.offset,
                    },
                })
            }
            Self::PlayContext => {
                let input: PlayContextArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::Play {
                        target: input.target,
                    },
                })
            }
            Self::Seek => {
                let input: SeekArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::Seek {
                        context_token: input.context_token,
                        position_ms: input.position_ms,
                    },
                })
            }
            Self::Volume => {
                let input: VolumeArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::Volume {
                        volume: input.volume,
                    },
                })
            }
            Self::Rating => {
                let input: RatingArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::Rate {
                        target: input.target,
                        rating: input.rating,
                    },
                })
            }
            Self::Append => {
                let input: AppendArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::AddPlaylistEntries {
                        playlist_id: input.playlist_id,
                        created_by_operation_id: input.created_by_operation_id,
                        video_ids: input.video_ids,
                    },
                })
            }
            Self::CreatePlaylist => {
                let input: CreatePlaylistArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::CreatePrivatePlaylist {
                        title: input.title,
                        description: input.description,
                    },
                })
            }
            Self::Enqueue => {
                let input: EnqueueArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: WriteAction::Enqueue {
                        context_token: input.context_token,
                        mode: input.mode,
                        video_ids: input.video_ids,
                    },
                })
            }
            Self::Playback => {
                let input: PlaybackArguments = serde_json::from_value(value)?;
                ControlRequest::Submit(WriteInput {
                    operation_id: input.operation_id,
                    preconditions: input.preconditions,
                    action: match input.action {
                        PlaybackAction::Resume => WriteAction::Resume,
                        PlaybackAction::Pause => WriteAction::Pause,
                        PlaybackAction::Stop => WriteAction::Stop,
                        PlaybackAction::Next => WriteAction::Next,
                        PlaybackAction::Previous => WriteAction::Previous,
                    },
                })
            }
            Self::Playlist => ControlRequest::GetPlaylist(serde_json::from_value(value)?),
            Self::ShareLink => ControlRequest::GetShareLink(serde_json::from_value(value)?),
            Self::Recommendations => ControlRequest::Recommend(serde_json::from_value(value)?),
            Self::Account => {
                let _: NoInput = serde_json::from_value(value)?;
                ControlRequest::GetAccount
            }
            Self::Capabilities => {
                let input: CapabilitiesInput = serde_json::from_value(value)?;
                ControlRequest::GetCapabilities { area: input.area }
            }
            Self::Context => ControlRequest::GetContext(serde_json::from_value(value)?),
            Self::Queue => ControlRequest::GetQueue(serde_json::from_value(value)?),
            Self::Search => {
                let input: SearchArguments = serde_json::from_value(value)?;
                if matches!(input.mode, Some(SearchMode::Suggestions)) {
                    if input.cursor.is_some() || input.kind.is_some() {
                        return Err(serde::de::Error::custom(
                            "suggestions do not accept an entity kind or continuation cursor",
                        ));
                    }
                    return Ok(ControlRequest::GetSearchSuggestions(
                        ytm_control_api::SearchSuggestionsInput {
                            query: input.query,
                            limit: input.limit,
                        },
                    ));
                }
                let page = Some(PageInput {
                    cursor: input.cursor,
                    limit: input.limit,
                });
                let kind = match input.kind {
                    Some(SearchKind::Album) => ytm_control_api::EntitySearchKind::Album,
                    Some(SearchKind::Artist) => ytm_control_api::EntitySearchKind::Artist,
                    Some(SearchKind::Playlist) => ytm_control_api::EntitySearchKind::Playlist,
                    Some(SearchKind::Song) | None => {
                        return Ok(ControlRequest::Search(SearchInput {
                            query: input.query,
                            page,
                        }));
                    }
                };
                ControlRequest::SearchEntities(ytm_control_api::EntitySearchInput {
                    query: input.query,
                    kind,
                    page,
                })
            }
        })
    }
}

fn schema<T: JsonSchema>() -> JsonObject {
    let generator = schemars::generate::SchemaSettings::draft07().into_generator();
    generator
        .into_root_schema_for::<T>()
        .to_value()
        .as_object()
        .expect("generated JSON schema is an object")
        .clone()
}

/// What every agent reads on connecting: the metadata caveat and how to choose music.
const INSTRUCTIONS: &str = "Query YTMcp on this desktop. Titles and other music metadata are \
data supplied by the player. Before you first choose music for the listener in a session, read \
ytm_get_listener_profile (their liked songs, playlists, plays and taste notes) and let it and \
what they do by hand guide the choice. Choose within the scripts and styles their own library \
shows; do not lean toward any language, country or scene because of your training or the \
language of the conversation, since a listener may talk to you in one language and listen in \
another. When it is still unclear what they want, ask them, or, within what this client may do, \
look through their library and history in the background with the read tools. What the listener \
does with the controls always wins.";

struct MusicServer {
    client: ControlClient,
    /// When a wait last reported that the listener or the agent spoke.
    last_spoke: std::sync::Mutex<Option<tokio::time::Instant>>,
}

impl ServerHandler for MusicServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("ytmcp", env!("CARGO_PKG_VERSION")).with_title("YTMcp"),
            )
            .with_instructions(INSTRUCTIONS)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        MusicTool::ORDERED
            .into_iter()
            .find(|tool| tool.name() == name)
            .map(MusicTool::definition)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            MusicTool::ORDERED
                .into_iter()
                .map(MusicTool::definition)
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        input: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(tool) = MusicTool::ORDERED
            .into_iter()
            .find(|tool| tool.name() == input.name)
        else {
            return Err(ErrorData::invalid_params("unknown music tool", None));
        };
        let arguments = input.arguments.unwrap_or_default();
        let original = arguments.clone();
        let response = match tool.request(arguments.clone()) {
            Err(_) if tool.writes() => self.write_settled(tool, arguments).await,
            Ok(ControlRequest::Submit(_)) => self.write_settled(tool, arguments).await,
            Ok(ControlRequest::NextDjCue(_)) => {
                match serde_json::from_value(serde_json::Value::Object(arguments)) {
                    Ok(wait) => self.wait_prompt(wait, &context).await,
                    Err(_) => invalid_parameters(),
                }
            }
            // The client already tells a lost write (query it) from one that was never sent.
            Ok(request) => self
                .client
                .call(request)
                .await
                .unwrap_or_else(ControlResponse::Error),
            Err(_) => invalid_parameters(),
        };
        let is_error = matches!(response, ControlResponse::Error(_));
        let payload = serde_json::to_value(&response)
            .map_err(|_| ErrorData::internal_error("invalid music control response", None))?;
        let text = render_tool_content(tool, &original, &response, &payload);
        let mut result = if is_error {
            CallToolResult::structured_error(payload)
        } else {
            CallToolResult::structured(payload)
        };
        result.content = vec![ContentBlock::text(text)];
        Ok(result.into())
    }
}

/// Largest read result, in bytes of JSON, that the text content repeats after its summary line.
const TEXT_JSON_LIMIT: usize = 48 * 1024;

/// The page's own continuation in a read summary, with a leading space; empty when not read.
fn official_summary(queue: &ytm_control_api::QueueProjection) -> String {
    queue
        .official_upcoming
        .as_ref()
        .map(|official| {
            let tracks: Vec<_> = official
                .tracks
                .iter()
                .take(4)
                .map(ytm_control_api::DjTrack::from)
                .collect();
            let filter = queue
                .filters
                .iter()
                .find(|filter| filter.selected)
                .map(|filter| filter.label.as_str());
            format!(
                " {}",
                outcome::official_text(
                    official.status,
                    &tracks,
                    official.more || official.tracks.len() > tracks.len(),
                    filter
                )
            )
        })
        .unwrap_or_default()
}

/// The playback sentence of a player or context read; while the player is still preparing it
/// says so, because no track then does not mean nothing will play.
fn playback_text(state: &ytm_control_api::PlayerState) -> String {
    if state.preparing {
        format!(
            "Playback: {:?}. The player is still preparing (finding out what it can play); a resume sent now waits up to 10 s for it, and starting a track or collection is sent at once.",
            state.playback
        )
    } else {
        format!("Playback: {:?}.", state.playback)
    }
}

/// Renders a tool result's text content for clients that ignore structuredContent: one sentence
/// for writes and errors, the whole prompt for a DJ cue, and for reads a summary line followed by
/// the reply as one line of JSON, unless that JSON exceeds [`TEXT_JSON_LIMIT`].
fn render_tool_content(
    tool: MusicTool,
    original: &JsonObject,
    response: &ControlResponse,
    payload: &serde_json::Value,
) -> String {
    if tool.writes() {
        return outcome::write_text(tool, original, response);
    }
    match response {
        ControlResponse::Error(err) => outcome::read_error_text(err),
        ControlResponse::DjCue(reply) => match &reply.cue {
            Some(cue) => outcome::cue_text(cue),
            None => "No DJ prompt yet. Call ytm_wait_dj_prompt again.".to_owned(),
        },
        _ => {
            let summary = match response {
                ControlResponse::Account(account) => format!("Account: {:?}.", account.state),
                ControlResponse::Capabilities(items) => {
                    format!("{} capability records.", items.len())
                }
                ControlResponse::Context(snapshot) => {
                    let upcoming = if snapshot.queue.upcoming == UpcomingStatus::Unknown {
                        " What follows the current track is not known right now (it is not in the loaded queue window)."
                    } else {
                        ""
                    };
                    let upcoming = format!("{upcoming}{}", official_summary(&snapshot.queue));
                    let actions = outcome::listener_actions_text(&snapshot.listener_actions, 0)
                        .map(|line| format!(" {line}"))
                        .unwrap_or_default();
                    let playback = playback_text(&snapshot.player);
                    if let Some(moment) = &snapshot.moment {
                        let extra = outcome::moment_text_lines(moment).join(" ");
                        format!("{playback}{upcoming} {extra}{actions}")
                    } else {
                        format!("{playback}{upcoming}{actions}")
                    }
                }
                ControlResponse::Player(player) => match &player.observed {
                    Some(state) => playback_text(state),
                    None => "Audio observations from the player.".into(),
                },
                ControlResponse::Queue(queue) => format!(
                    "{} entries in the loaded window. Completeness: {:?}.{}{}",
                    queue.entries.len(),
                    queue.completeness,
                    if queue.upcoming == UpcomingStatus::Unknown {
                        " The current track is not in the loaded window."
                    } else {
                        ""
                    },
                    official_summary(queue)
                ),
                ControlResponse::Search(page) => format!(
                    "{} catalog results. Freshness: {:?}.",
                    page.items.len(),
                    page.freshness
                ),
                // Only writes return receipts, and they are summarized above.
                ControlResponse::Operation(_) => outcome::UNCLEAR.to_owned(),
                ControlResponse::Playlist(playlist) => {
                    format!("{} playlist entries.", playlist.entries.items.len())
                }
                ControlResponse::ShareLink(link) => {
                    format!("YouTube Music share link ({:?}): {}", link.access, link.url)
                }
                ControlResponse::Track(track) => {
                    format!("Observed track with rating {:?}.", track.track.rating)
                }
                ControlResponse::Discovery(discovery) => format!(
                    "{} discovery items from {}.",
                    discovery.items.items.len(),
                    discovery.items.source
                ),
                ControlResponse::ExtendedMedia(media) => format!(
                    "{} media entries from {}.",
                    media.entries.items.len(),
                    media.entries.source
                ),
                ControlResponse::Album(album) => {
                    format!("{} tracks in this album window.", album.tracks.len())
                }
                ControlResponse::Artist(artist) => format!(
                    "{} songs and {} albums in this artist overview.",
                    artist.top_songs.len(),
                    artist.albums.len()
                ),
                ControlResponse::History(page) => {
                    format!("{} recent music entries.", page.items.len())
                }
                ControlResponse::HistoryEntries(page) => {
                    format!("{} recent entries from {}.", page.items.len(), page.source)
                }
                ControlResponse::SearchSuggestions(page) => {
                    format!("{} public search suggestions.", page.items.len())
                }
                ControlResponse::SearchEntities(page) => format!(
                    "{} matching entities from {}.",
                    page.items.len(),
                    page.source
                ),
                ControlResponse::LocalHistory(history) => {
                    format!("{} local history entries.", history.entries.items.len())
                }
                ControlResponse::CombinedHistory(history) => format!(
                    "{} local entries and {} YouTube entries.",
                    history.local.entries.items.len(),
                    history.youtube.as_ref().map_or(0, |page| page.items.len())
                ),
                ControlResponse::Home(home) => format!("Discovery source {}.", home.source),
                ControlResponse::Library(_) => "Music library window.".to_owned(),
                ControlResponse::Settings(settings) => {
                    format!("Device settings revision {}.", settings.settings_revision)
                }
                ControlResponse::Lyrics(page) => {
                    format!("Lyrics: {:?}, source {}.", page.kind, page.source)
                }
                ControlResponse::ListenerProfile(profile) => outcome::profile_text(profile),
                ControlResponse::DjPreferences(page) => {
                    format!("{} DJ preferences in this page.", page.items.len())
                }
                ControlResponse::Recommendations(page) => {
                    format!("{} YouTube Music candidates.", page.candidates.items.len())
                }
                ControlResponse::Error(_) | ControlResponse::DjCue(_) => unreachable!(),
            };
            match serde_json::to_string(payload) {
                Ok(json) if json.len() <= TEXT_JSON_LIMIT => format!("{summary}\n{json}"),
                Ok(_) => format!(
                    "{summary} The full result is in structuredContent; it is too large to repeat \
                     here, so ask for a smaller page with limit to read it as text."
                ),
                Err(_) => summary,
            }
        }
    }
}

const BOOKKEEPING: [(&str, &str); 3] = [
    ("operationId", "Optional."),
    ("preconditions", "Optional."),
    ("contextToken", "Optional."),
];

/// Marks the journal bookkeeping of write tools optional in their published input schemas,
/// including the context token of a `ytm_play_context` queue occurrence target.
fn optional_bookkeeping(mut schema: JsonObject) -> JsonObject {
    relax_bookkeeping(&mut schema);
    let queue_entry = schema
        .get_mut("definitions")
        .and_then(|definitions| definitions.get_mut("PlayContextTarget"))
        .and_then(|target| target.get_mut("oneOf"))
        .and_then(serde_json::Value::as_array_mut)
        .into_iter()
        .flatten()
        .find(|variant| {
            variant
                .pointer("/properties/kind/const")
                .and_then(serde_json::Value::as_str)
                == Some("queueEntry")
        })
        .and_then(serde_json::Value::as_object_mut);
    if let Some(variant) = queue_entry {
        relax_bookkeeping(variant);
    }
    schema
}

fn relax_bookkeeping(schema: &mut JsonObject) {
    if let Some(serde_json::Value::Array(required)) = schema.get_mut("required") {
        required.retain(|name| {
            !BOOKKEEPING
                .iter()
                .any(|(field, _)| name.as_str() == Some(*field))
        });
    }
    if let Some(serde_json::Value::Object(properties)) = schema.get_mut("properties") {
        for (field, text) in BOOKKEEPING {
            if let Some(serde_json::Value::Object(property)) = properties.get_mut(field) {
                property.insert("description".into(), (*text).into());
            }
        }
    }
}

/// How long a write call waits for its operation to settle before returning the receipt as is.
const SETTLE_WAIT: std::time::Duration = std::time::Duration::from_secs(12);
const SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(300);

/// How long a write whose reply was lost keeps looking for its operation.
const RECOVER_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The bookkeeping a write call left out, which the adapter fills from the current context.
struct Missing {
    token: bool,
    /// The context token of a `ytm_play_context` queue occurrence target.
    target_token: bool,
    preconditions: bool,
    /// Whether the write is checked against the queue revision it was captured with.
    queue: bool,
}

impl Missing {
    fn of(tool: MusicTool, arguments: &JsonObject) -> Self {
        let target = arguments
            .get("target")
            .and_then(serde_json::Value::as_object)
            .filter(|target| {
                matches!(tool, MusicTool::PlayContext)
                    && target.get("kind").and_then(serde_json::Value::as_str) == Some("queueEntry")
            });
        Self {
            token: tool.accepts("contextToken") && !arguments.contains_key("contextToken"),
            target_token: target.is_some_and(|target| !target.contains_key("contextToken")),
            preconditions: tool.accepts("preconditions")
                && !arguments.contains_key("preconditions"),
            // An Up Next filter changes only the suggestions, so a queue edit does not stale it.
            queue: tool.accepts("contextToken")
                || target.is_some()
                || (matches!(tool, MusicTool::PlaybackMode)
                    && arguments
                        .get("change")
                        .and_then(|change| change.get("kind"))
                        .and_then(serde_json::Value::as_str)
                        != Some("upNextFilter")),
        }
    }

    fn any(&self) -> bool {
        self.token || self.target_token || self.preconditions
    }
}

impl MusicServer {
    /// Submits a write, filling in what the caller left out from the current state, and returns
    /// the receipt once it settles. A refusal, stale or not, is returned as is: the agent re-reads
    /// and decides, since the user may have taken over in the meantime.
    async fn write_settled(&self, tool: MusicTool, mut arguments: JsonObject) -> ControlResponse {
        let missing = Missing::of(tool, &arguments);
        // The journal binds an ID to its exact request and filled-in state differs between calls,
        // so verify caller-supplied business intent via ReplayOperation hashes before returning an
        // existing receipt (or returning IDEMPOTENCY_CONFLICT if parameters differ).
        if missing.any()
            && let Some(id) = arguments
                .get("operationId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        {
            let mut probe = arguments.clone();
            if missing.token {
                probe.insert("contextToken".into(), "".into());
            }
            if missing.target_token
                && let Some(serde_json::Value::Object(target)) = probe.get_mut("target")
            {
                target.insert("contextToken".into(), "".into());
            }
            if missing.preconditions {
                probe.insert("preconditions".into(), serde_json::json!({}));
            }
            let Ok(ControlRequest::Submit(probe_input)) = tool.request(probe) else {
                return invalid_parameters();
            };
            let (exact_hash, tokenless_hash) =
                ytm_control_api::compute_action_hashes(&probe_input.action);
            let token_missing = missing.token || missing.target_token;
            let replay = ytm_control_api::OperationReplayInput {
                operation_id: id,
                action_hash: if token_missing {
                    tokenless_hash
                } else {
                    exact_hash
                },
                token_was_missing: token_missing,
                explicit_preconditions: (!missing.preconditions)
                    .then_some(probe_input.preconditions),
            };
            match self
                .client
                .call(ControlRequest::ReplayOperation(replay))
                .await
            {
                Ok(ControlResponse::Operation(receipt)) => return self.settle(receipt).await,
                Ok(ControlResponse::Error(err)) | Err(err)
                    if err.code == ErrorCode::InvalidInput => {}
                Ok(ControlResponse::Error(err)) | Err(err) => {
                    return ControlResponse::Error(err);
                }
                Ok(_) => {}
            }
        }
        if let Err(error) = self.fill_bookkeeping(tool, &missing, &mut arguments).await {
            return ControlResponse::Error(error);
        }
        let id = arguments
            .get("operationId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Ok(request) = tool.request(arguments) else {
            return invalid_parameters();
        };
        match self.client.call(request).await {
            Ok(ControlResponse::Operation(receipt)) => self.settle(receipt).await,
            Ok(other) => other,
            Err(error) if error.retry_disposition == RetryDisposition::QueryOperation => {
                self.recover(&id, error).await
            }
            Err(error) => ControlResponse::Error(error),
        }
    }

    /// Reads an operation of this client, or `None` when the player has no operation by this ID.
    async fn operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<ytm_control_api::OperationReceipt>, ControlError> {
        match self
            .client
            .call(ControlRequest::GetOperation(OperationInput {
                operation_id: operation_id.to_owned(),
            }))
            .await?
        {
            ControlResponse::Operation(receipt) => Ok(Some(receipt)),
            ControlResponse::Error(error) if error.code == ErrorCode::InvalidInput => Ok(None),
            ControlResponse::Error(error) => Err(error),
            _ => Err(ControlError::new(
                ErrorCode::Unavailable,
                RetryDisposition::SafeReadRetry,
                "the player did not return the operation",
            )),
        }
    }

    /// Looks up a write that was sent but whose reply was lost, so the agent gets its receipt
    /// instead of an ID it never saw. The player may record it shortly after the connection drops.
    async fn recover(&self, operation_id: &str, mut lost: ControlError) -> ControlResponse {
        let deadline = tokio::time::Instant::now() + RECOVER_WAIT;
        loop {
            match self.operation(operation_id).await {
                Ok(Some(receipt)) => return self.settle(receipt).await,
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(SETTLE_POLL).await;
                }
                Ok(None) | Err(_) => break,
            }
        }
        lost.message = format!("{}; its operationId is {operation_id}", lost.message);
        ControlResponse::Error(lost)
    }

    async fn fill_bookkeeping(
        &self,
        tool: MusicTool,
        missing: &Missing,
        arguments: &mut JsonObject,
    ) -> Result<(), ControlError> {
        if tool.accepts("operationId") && !arguments.contains_key("operationId") {
            let id = format!(
                "auto-{:016x}{:016x}",
                rand::random::<u64>(),
                rand::random::<u64>()
            );
            arguments.insert("operationId".into(), id.into());
        }
        if !missing.any() {
            return Ok(());
        }
        let context = match self
            .client
            .call(ControlRequest::GetContext(ContextInput {
                preview_limit: Some(0),
            }))
            .await
        {
            Ok(ControlResponse::Context(context)) => context,
            Ok(ControlResponse::Error(error)) | Err(error) => return Err(error),
            Ok(_) => {
                return Err(ControlError::new(
                    ErrorCode::Unavailable,
                    RetryDisposition::SafeReadRetry,
                    "the player did not return its current context",
                ));
            }
        };
        if missing.token {
            arguments.insert("contextToken".into(), context.context_token.clone().into());
        }
        if missing.target_token
            && let Some(serde_json::Value::Object(target)) = arguments.get_mut("target")
        {
            target.insert("contextToken".into(), context.context_token.clone().into());
        }
        if missing.preconditions {
            // Queue placement checks the queue it was captured against; other writes need not.
            let value = serde_json::json!({
                "expectedAccountEpoch": context.account.account_epoch,
                "expectedBridgeEpoch": context.bridge_epoch,
                "expectedQueueRevision": missing.queue.then_some(context.queue.revision),
            });
            arguments.insert("preconditions".into(), value);
        }
        Ok(())
    }

    /// Polls an accepted operation until it stops scheduling work, an interruption is playing
    /// and waiting to resume, or `SETTLE_WAIT` passes.
    async fn settle(&self, mut receipt: ytm_control_api::OperationReceipt) -> ControlResponse {
        let deadline = tokio::time::Instant::now() + SETTLE_WAIT;
        loop {
            let resuming = matches!(
                receipt.resource,
                Some(ytm_control_api::OperationResource::Interruption {
                    resume: ytm_control_api::InterruptResumeState::Pending,
                    ..
                })
            ) && receipt.phase != ytm_control_api::OperationPhase::Accepted
                && receipt.phase != ytm_control_api::OperationPhase::Submitted;
            if receipt.scheduling_stopped || resuming || tokio::time::Instant::now() >= deadline {
                return ControlResponse::Operation(receipt);
            }
            tokio::time::sleep(SETTLE_POLL).await;
            match self
                .client
                .call(ControlRequest::GetOperation(OperationInput {
                    operation_id: receipt.operation_id.clone(),
                }))
                .await
            {
                Ok(ControlResponse::Operation(next)) => receipt = next,
                _ => return ControlResponse::Operation(receipt),
            }
        }
    }
}

fn invalid_parameters() -> ControlResponse {
    ControlResponse::Error(ControlError::new(
        ErrorCode::InvalidInput,
        RetryDisposition::NotRetryable,
        "invalid parameters for this music tool",
    ))
}

/// How long a conversation stays hot after the listener or the agent last spoke.
const HOT_CONVERSATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// The longest wait while the conversation is hot, so the agent soon reads the listener's answer.
const HOT_WAIT_SECONDS: u64 = 60;

/// The seconds a wait lasts: the requested time, capped while the conversation is hot.
fn wait_seconds(
    requested: Option<u32>,
    last_spoke: Option<tokio::time::Instant>,
    now: tokio::time::Instant,
) -> u64 {
    let wait = u64::from(requested.unwrap_or(1200).clamp(1, 1800));
    if last_spoke.is_some_and(|at| now.saturating_duration_since(at) < HOT_CONVERSATION) {
        wait.min(HOT_WAIT_SECONDS)
    } else {
        wait
    }
}

/// How often a waiting call asks the player for a prompt.
const PROMPT_POLL: std::time::Duration = std::time::Duration::from_secs(1);
/// Progress notifications keep clients with short per-call timeouts (pi's is 60 s) waiting.
const PROMPT_PROGRESS: std::time::Duration = std::time::Duration::from_secs(20);

impl MusicServer {
    /// Waits for the next DJ prompt by polling the player, which records the agent's report and
    /// acknowledgement on the first poll. Returns the prompt, an error, or an empty reply at the
    /// deadline.
    async fn wait_prompt(
        &self,
        input: WaitPromptArguments,
        context: &RequestContext<RoleServer>,
    ) -> ControlResponse {
        use tokio::time::Instant;
        let now = Instant::now();
        let last_spoke = {
            let mut last = self
                .last_spoke
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if input.user_spoke || input.spoke {
                *last = Some(now);
            }
            *last
        };
        let wait = wait_seconds(input.max_wait_seconds, last_spoke, now);
        let deadline = now + std::time::Duration::from_secs(wait);
        let token = context.meta.get_progress_token();
        let mut report = ytm_control_api::DjCueInput {
            user_spoke: input.user_spoke,
            spoke: input.spoke,
            chattiness: input.chattiness,
            last_received: input.last_received,
        };
        let (mut last_progress, mut sent) = (Instant::now(), 0_u32);
        loop {
            let poll = tokio::select! {
                biased;
                () = context.ct.cancelled() => {
                    return ControlResponse::DjCue(Box::new(ytm_control_api::DjCueReply {
                        cue: None,
                        chattiness: ytm_control_api::DjChattiness::default(),
                    }));
                }
                res = self
                    .client
                    .call_attach_only(ControlRequest::NextDjCue(std::mem::take(&mut report))) => res,
            };
            let response = poll.unwrap_or_else(ControlResponse::Error);
            let empty = matches!(&response, ControlResponse::DjCue(reply) if reply.cue.is_none());
            if !empty || Instant::now() >= deadline {
                return response;
            }
            if let Some(token) = token.clone()
                && last_progress.elapsed() >= PROMPT_PROGRESS
            {
                sent += 1;
                last_progress = Instant::now();
                let _ = context
                    .peer
                    .notify_progress(
                        rmcp::model::ProgressNotificationParam::new(token, f64::from(sent))
                            .with_message("waiting for the next DJ prompt"),
                    )
                    .await;
            }
            tokio::select! {
                () = context.ct.cancelled() => return response,
                () = tokio::time::sleep(PROMPT_POLL) => {}
            }
        }
    }
}

fn parse_args() -> Result<(PathBuf, bool), &'static str> {
    let mut args = std::env::args_os().skip(1);
    let mut credential = None;
    let mut allow_start = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--credential-file") if credential.is_none() => {
                credential = args.next().map(PathBuf::from)
            }
            // Every tool is published; the old profile choice is accepted and ignored.
            Some("--profile")
                if matches!(
                    args.next().as_deref().and_then(std::ffi::OsStr::to_str),
                    Some("common" | "development")
                ) => {}
            Some("--allow-background-start") => allow_start = true,
            Some("--socket") if args.next().as_deref() == Some(std::ffi::OsStr::new("auto")) => {}
            _ => return Err(USAGE),
        }
    }
    let path = credential.filter(|path| path.is_absolute()).ok_or(USAGE)?;
    Ok((path, allow_start))
}

#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("ytmcp: {message}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|value| value == "register")
    {
        return register::register();
    }
    let (credential, allow_start) = parse_args().map_err(str::to_owned)?;
    let client = ControlClient::new(credential, allow_start);
    let (input, output) = rmcp::transport::stdio();
    let transport = rmcp::transport::async_rw::AsyncRwTransport::new(input, output)
        .with_max_line_length(ytm_control_api::MAX_FRAME_BYTES);
    MusicServer {
        client,
        last_spoke: std::sync::Mutex::new(None),
    }
    .serve(transport)
    .await
    .map_err(|_| "MCP initialization failed".to_owned())?
    .waiting()
    .await
    .map_err(|_| "MCP stdio transport failed".to_owned())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, Instant};

    #[test]
    fn a_hot_conversation_caps_each_wait_for_five_minutes() {
        let now = Instant::now();
        let spoke = Some(now);
        assert_eq!(wait_seconds(None, None, now), 1200);
        assert_eq!(wait_seconds(Some(1800), spoke, now), 60);
        assert_eq!(wait_seconds(Some(5), spoke, now), 5);
        let later = now + Duration::from_secs(299);
        assert_eq!(wait_seconds(None, spoke, later), 60);
        let cooled = now + Duration::from_secs(300);
        assert_eq!(wait_seconds(None, spoke, cooled), 1200);
    }

    #[test]
    fn the_up_next_filter_mode_selects_a_filter_by_its_id() {
        let arguments = serde_json::json!({
            "operationId": "fixture-filter",
            "preconditions": { "expectedBridgeEpoch": "fixture-bridge" },
            "change": { "kind": "upNextFilter", "filterId": "qf-1-c" }
        });
        let serde_json::Value::Object(arguments) = arguments else {
            unreachable!()
        };
        assert!(!Missing::of(MusicTool::PlaybackMode, &arguments).queue);
        let Ok(ControlRequest::Submit(input)) = MusicTool::PlaybackMode.request(arguments) else {
            panic!("expected a write");
        };
        assert_eq!(
            input.action,
            WriteAction::SetQueueFilter {
                filter_id: "qf-1-c".into()
            }
        );
    }

    #[test]
    fn the_operation_ledger_is_not_published() {
        let names: Vec<_> = MusicTool::ORDERED
            .into_iter()
            .map(MusicTool::name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for hidden in [
            "ytm_get_operation",
            "ytm_cancel_operation",
            "ytm_resume_operation",
        ] {
            assert!(!names.contains(&hidden), "{hidden}");
        }
    }

    #[test]
    fn write_descriptions_keep_bookkeeping_internal() {
        for tool in MusicTool::ORDERED.into_iter().filter(|tool| tool.writes()) {
            let definition = tool.definition();
            let text = definition.description.unwrap_or_default();
            for word in ["journal", "epoch", "operation", "precondition"] {
                assert!(!text.contains(word), "{} mentions {word}", tool.name());
            }
            let properties = &definition.input_schema["properties"];
            for (field, _) in BOOKKEEPING {
                if let Some(property) = properties.get(field) {
                    assert_eq!(property["description"], "Optional.", "{}", tool.name());
                }
            }
        }
    }

    #[test]
    fn server_info_identifies_ytmcp_and_crate_version() {
        let server = MusicServer {
            client: ControlClient::new(PathBuf::from("/nonexistent"), false),
            last_spoke: std::sync::Mutex::new(None),
        };
        let info = server.get_info();
        assert_eq!(info.server_info.name, "ytmcp");
        assert_eq!(info.server_info.title.as_deref(), Some("YTMcp"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn read_tool_content_includes_parseable_data_without_structured_content() {
        use ytm_control_api::{ShareAccess, ShareLink};

        let search_resp: ControlResponse = serde_json::from_value(serde_json::json!({
            "result": "search",
            "value": {
                "items": [{
                    "videoId": "dQw4w9WgXcQ",
                    "title": "Fixture Track",
                    "artists": [{ "id": null, "name": "Fixture Artist" }],
                    "album": null,
                    "durationMs": 180000,
                    "availability": "available",
                    "versionTag": null,
                    "rating": "neutral"
                }],
                "nextCursor": "cursor-page-2",
                "complete": false,
                "source": "fixture",
                "freshness": "fresh",
                "capturedAtMs": 1000
            }
        }))
        .unwrap();
        let search_payload = serde_json::to_value(&search_resp).unwrap();
        let search_text = render_tool_content(
            MusicTool::Search,
            &JsonObject::new(),
            &search_resp,
            &search_payload,
        );
        // Simulate a content-only harness discarding structuredContent.
        let json_line = search_text.split_once('\n').expect("summary + json").1;
        let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(
            parsed["value"]["items"][0]["videoId"],
            serde_json::json!("dQw4w9WgXcQ")
        );
        assert_eq!(
            parsed["value"]["items"][0]["title"],
            serde_json::json!("Fixture Track")
        );
        assert_eq!(
            parsed["value"]["nextCursor"],
            serde_json::json!("cursor-page-2")
        );

        let share_resp = ControlResponse::ShareLink(ShareLink {
            url: "https://music.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
            visibility: None,
            access: ShareAccess::Unlisted,
        });
        let share_payload = serde_json::to_value(&share_resp).unwrap();
        let share_text = render_tool_content(
            MusicTool::ShareLink,
            &JsonObject::new(),
            &share_resp,
            &share_payload,
        );
        assert!(share_text.contains("https://music.youtube.com/watch?v=dQw4w9WgXcQ"));

        let cue_resp = ControlResponse::DjCue(Box::new(ytm_control_api::DjCueReply {
            cue: None,
            chattiness: ytm_control_api::DjChattiness::Normal,
        }));
        let cue_payload = serde_json::to_value(&cue_resp).unwrap();
        let cue_text = render_tool_content(
            MusicTool::WaitPrompt,
            &JsonObject::new(),
            &cue_resp,
            &cue_payload,
        );
        assert!(!cue_text.contains('\n'));
    }

    #[test]
    fn queue_read_text_names_the_official_continuation_and_selected_filter() {
        let queue: ControlResponse = serde_json::from_value(serde_json::json!({
            "result": "queue",
            "value": {
                "revision": 4, "currentEntryId": null, "upcoming": "unknown", "entries": [],
                "nextCursor": null, "completeness": "partial", "hasMore": true, "infinite": null,
                "loadedStart": null, "loadedEndExclusive": null,
                "officialUpcoming": {
                    "status": "observed",
                    "tracks": [{
                        "videoId": "autoAAAAAAA", "title": "Auto A",
                        "artists": [{ "id": null, "name": "Artist A" }], "album": null,
                        "durationMs": null, "availability": "available", "versionTag": null,
                        "rating": "unknown"
                    }],
                    "more": false
                },
                "filters": [
                    { "filterId": "qf-0-a", "label": "All", "selected": false },
                    { "filterId": "qf-1-b", "label": "Deep cuts", "selected": true }
                ]
            }
        }))
        .unwrap();
        let payload = serde_json::to_value(&queue).unwrap();
        let text = render_tool_content(MusicTool::Queue, &JsonObject::new(), &queue, &payload);
        let summary = text.split_once('\n').expect("summary + json").0;
        assert!(
            summary.ends_with(
                "The current track is not in the loaded window. Then YouTube Music: Auto A by \
                 Artist A (filter: Deep cuts)."
            ),
            "{summary}"
        );
    }

    #[test]
    fn context_read_with_no_preview_still_says_suggestions_follow() {
        let context: ControlResponse = serde_json::from_value(serde_json::json!({
            "result": "context",
            "value": {
                "cursor": { "coreEpoch": "fixture-core", "revision": 1 },
                "bridgeEpoch": null, "capturedAtMs": 1000, "contextToken": "fixture-token",
                "freshness": "fresh",
                "player": {
                    "connection": "ready", "playback": "playing", "trackInstanceId": null,
                    "track": null,
                    "timeAnchor": { "positionMs": 0, "playbackRate": 1.0,
                                    "anchorCoreMonotonicMs": 0, "extrapolating": false },
                    "volume": 1.0, "muted": false, "shuffle": null, "repeat": null,
                    "autoplay": true, "activeOp": null, "pendingCommands": 0,
                    "stoppedConfirmed": false
                },
                "account": { "state": "authenticated", "accountEpoch": null,
                             "playerCatalogMatched": true },
                "queue": {
                    "revision": 4, "currentEntryId": null, "upcoming": "known", "entries": [],
                    "nextCursor": null, "completeness": "partial", "hasMore": true,
                    "infinite": null, "loadedStart": null, "loadedEndExclusive": null,
                    "officialUpcoming": { "status": "observed", "tracks": [], "more": true },
                    "filters": [{ "filterId": "qf-1-c", "label": "Familiar", "selected": true }]
                },
                "capabilities": []
            }
        }))
        .unwrap();
        let payload = serde_json::to_value(&context).unwrap();
        let text = render_tool_content(MusicTool::Context, &JsonObject::new(), &context, &payload);
        let summary = text.split_once('\n').expect("summary + json").0;
        assert_eq!(
            summary,
            "Playback: Playing. Then YouTube Music: its own suggestions (not listed) (filter: \
             Familiar)."
        );
    }

    /// A read made while the player is still preparing says so on its summary line, so an
    /// agent that sees no track right after launch knows a play will be held, not refused.
    #[test]
    fn player_read_text_says_when_the_player_is_still_preparing() {
        let player = |preparing: bool| -> ControlResponse {
            serde_json::from_value(serde_json::json!({
                "result": "player",
                "value": {
                    "bridgeEpoch": null, "freshness": "fresh", "pending": null, "audio": null,
                    "observed": {
                        "connection": "ready", "playback": "idle", "trackInstanceId": null,
                        "track": null,
                        "timeAnchor": { "positionMs": 0, "playbackRate": 1.0,
                                        "anchorCoreMonotonicMs": 0, "extrapolating": false },
                        "volume": 1.0, "muted": false, "shuffle": null, "repeat": null,
                        "autoplay": null, "activeOp": null, "pendingCommands": 0,
                        "stoppedConfirmed": false, "preparing": preparing
                    }
                }
            }))
            .unwrap()
        };
        let summary = |response: &ControlResponse| {
            let payload = serde_json::to_value(response).unwrap();
            let text =
                render_tool_content(MusicTool::Player, &JsonObject::new(), response, &payload);
            (
                text.split_once('\n').expect("summary + json").0.to_owned(),
                payload,
            )
        };
        let (text, payload) = summary(&player(true));
        assert_eq!(
            text,
            "Playback: Idle. The player is still preparing (finding out what it can play); a \
             resume sent now waits up to 10 s for it, and starting a track or collection is sent \
             at once."
        );
        assert_eq!(payload["value"]["observed"]["preparing"], true);
        let (text, payload) = summary(&player(false));
        assert_eq!(text, "Playback: Idle.");
        assert!(payload["value"]["observed"].get("preparing").is_none());
    }

    #[test]
    fn the_listener_profile_is_a_read_that_maps_to_its_request() {
        let tool = MusicTool::ORDERED
            .into_iter()
            .find(|tool| tool.name() == "ytm_get_listener_profile")
            .expect("published");
        assert!(tool.read_only() && !tool.writes());
        let serde_json::Value::Object(refresh) = serde_json::json!({ "refresh": true }) else {
            unreachable!()
        };
        assert_eq!(
            tool.request(refresh).unwrap(),
            ControlRequest::GetListenerProfile(ytm_control_api::ListenerProfileInput {
                refresh: true
            })
        );
        assert_eq!(
            tool.request(JsonObject::new()).unwrap(),
            ControlRequest::GetListenerProfile(Default::default())
        );
        let serde_json::Value::Object(unknown) = serde_json::json!({ "locale": "any" }) else {
            unreachable!()
        };
        assert!(tool.request(unknown).is_err());
    }

    #[test]
    fn context_read_text_lists_the_listeners_own_actions_on_its_summary_line() {
        let context: ControlResponse = serde_json::from_value(serde_json::json!({
            "result": "context",
            "value": {
                "cursor": { "coreEpoch": "fixture-core", "revision": 1 },
                "bridgeEpoch": null, "capturedAtMs": 1000, "contextToken": "fixture-token",
                "freshness": "fresh",
                "player": {
                    "connection": "ready", "playback": "paused", "trackInstanceId": null,
                    "track": null,
                    "timeAnchor": { "positionMs": 0, "playbackRate": 1.0,
                                    "anchorCoreMonotonicMs": 0, "extrapolating": false },
                    "volume": 0.3, "muted": false, "shuffle": null, "repeat": null,
                    "autoplay": null, "activeOp": null, "pendingCommands": 0,
                    "stoppedConfirmed": false
                },
                "account": { "state": "authenticated", "accountEpoch": null,
                             "playerCatalogMatched": true },
                "queue": {
                    "revision": 4, "currentEntryId": null, "upcoming": "known", "entries": [],
                    "nextCursor": null, "completeness": "complete", "hasMore": false,
                    "infinite": null, "loadedStart": null, "loadedEndExclusive": null
                },
                "capabilities": [],
                "listenerActions": [
                    { "action": "pause", "agoMs": 3000,
                      "during": { "videoId": "fixtureAAAA", "title": "Two\nlines",
                                  "artists": ["Band"], "album": null, "durationMs": null },
                      "positionMs": 42000 },
                    { "action": "volume", "agoMs": 8000, "volumeFrom": 70, "volumeTo": 30 }
                ]
            }
        }))
        .unwrap();
        let payload = serde_json::to_value(&context).unwrap();
        let text = render_tool_content(MusicTool::Context, &JsonObject::new(), &context, &payload);
        let (summary, json) = text.split_once('\n').expect("summary + json");
        assert_eq!(
            summary,
            "Playback: Paused. The listener's own actions, newest first: paused \"Two lines\" at \
             42 s; turned the volume down from 70 to 30."
        );
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(parsed["value"]["listenerActions"][1]["volumeTo"], 30);
    }

    #[test]
    fn oversized_read_text_keeps_only_the_summary() {
        let item = serde_json::json!({
            "videoId": "dQw4w9WgXcQ",
            "title": "t".repeat(1024),
            "artists": [],
            "album": null,
            "durationMs": 180000,
            "availability": "available",
            "versionTag": null,
            "rating": "neutral"
        });
        let response: ControlResponse = serde_json::from_value(serde_json::json!({
            "result": "search",
            "value": {
                "items": vec![item; 60],
                "nextCursor": null,
                "complete": true,
                "source": "fixture",
                "freshness": "fresh",
                "capturedAtMs": 1000
            }
        }))
        .unwrap();
        let payload = serde_json::to_value(&response).unwrap();
        assert!(payload.to_string().len() > TEXT_JSON_LIMIT);
        let text = render_tool_content(MusicTool::Search, &JsonObject::new(), &response, &payload);
        assert!(!text.contains('\n'), "{text}");
        assert!(text.starts_with("60 catalog results."), "{text}");
        assert!(text.contains("structuredContent"), "{text}");
    }
}
