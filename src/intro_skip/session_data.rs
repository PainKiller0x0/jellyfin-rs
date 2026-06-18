use std::time::Instant;

/// Per-play-session state for behavior-based intro/credits detection.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlaySessionData {
    pub item_id: String,
    pub user_id: String,
    pub client: String,

    // Existing markers (loaded from DB on session start)
    pub intro_start: Option<i64>,
    pub intro_end: Option<i64>,
    pub credits_start: Option<i64>,
    pub runtime_ticks: i64,

    // Tracking state
    pub playback_start_ticks: i64,
    pub previous_position_ticks: i64,
    pub previous_event_time: Instant,
    pub first_jump_position_ticks: Option<i64>,
    pub last_jump_position_ticks: Option<i64>,

    // Config (from StrmAssistantConfig)
    pub max_intro_duration_ticks: i64,
    pub max_credits_duration_ticks: i64,
    pub min_opening_plot_duration_ticks: i64,

    // Pause tracking
    pub last_pause_event_time: Option<Instant>,
    pub is_paused: bool,
}

impl PlaySessionData {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        item_id: String,
        user_id: String,
        client: String,
        runtime_ticks: i64,
        initial_position_ticks: i64,
        max_intro_duration_secs: i64,
        max_credits_duration_secs: i64,
        min_opening_plot_duration_secs: i64,
    ) -> Self {
        let ticks_per_sec = 10_000_000i64;
        Self {
            item_id,
            user_id,
            client,
            intro_start: None,
            intro_end: None,
            credits_start: None,
            runtime_ticks,
            playback_start_ticks: initial_position_ticks,
            previous_position_ticks: initial_position_ticks,
            previous_event_time: Instant::now(),
            first_jump_position_ticks: None,
            last_jump_position_ticks: None,
            max_intro_duration_ticks: max_intro_duration_secs * ticks_per_sec,
            max_credits_duration_ticks: max_credits_duration_secs * ticks_per_sec,
            min_opening_plot_duration_ticks: min_opening_plot_duration_secs * ticks_per_sec,
            last_pause_event_time: None,
            is_paused: false,
        }
    }
}
