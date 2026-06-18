use std::time::{Duration, Instant};

use dashmap::DashMap;

use super::session_data::PlaySessionData;

const TICKS_PER_SEC: i64 = 10_000_000;
const JUMP_THRESHOLD_SECS: f64 = 5.0;
const PAUSE_MIN_DURATION_MS: u64 = 500;
const PAUSE_MAX_DURATION_MS: u64 = 5000;
const UPDATE_COOLDOWN_SECS: u64 = 10;

/// Behavior-based intro/credits detector.
/// Monitors playback sessions and detects fast-forward jumps and pause-resume patterns.
pub struct IntroDetector {
    sessions: DashMap<String, PlaySessionData>,
    last_intro_update: DashMap<String, Instant>,
    last_credits_update: DashMap<String, Instant>,
    max_intro_duration_secs: i64,
    max_credits_duration_secs: i64,
    min_opening_plot_duration_secs: i64,
}

impl IntroDetector {
    pub fn new(
        max_intro_duration_secs: i64,
        max_credits_duration_secs: i64,
        min_opening_plot_duration_secs: i64,
    ) -> Self {
        Self {
            sessions: DashMap::new(),
            last_intro_update: DashMap::new(),
            last_credits_update: DashMap::new(),
            max_intro_duration_secs,
            max_credits_duration_secs,
            min_opening_plot_duration_secs,
        }
    }

    /// Called when a playback session starts.
    pub fn on_playback_start(
        &self,
        play_session_id: &str,
        item_id: &str,
        user_id: &str,
        client: &str,
        runtime_ticks: i64,
        position_ticks: i64,
    ) {
        let data = PlaySessionData::new(
            item_id.to_string(),
            user_id.to_string(),
            client.to_string(),
            runtime_ticks,
            position_ticks,
            self.max_intro_duration_secs,
            self.max_credits_duration_secs,
            self.min_opening_plot_duration_secs,
        );
        self.sessions.insert(play_session_id.to_string(), data);
    }

    /// Called on each playback progress event.
    pub fn on_playback_progress(
        &self,
        play_session_id: &str,
        position_ticks: i64,
        is_paused: bool,
    ) {
        // Collect all data needed from the session, then decide actions
        let (_item_id, intro_action, credits_action, _update_state) = {
            let Some(mut session) = self.sessions.get_mut(play_session_id) else {
                return;
            };

            let now = Instant::now();
            let elapsed_secs = now
                .duration_since(session.previous_event_time)
                .as_secs_f64();
            let position_delta_ticks = position_ticks - session.previous_position_ticks;
            let position_delta_secs = position_delta_ticks as f64 / TICKS_PER_SEC as f64;

            // Detect fast-forward jumps: |position_delta - wall_elapsed| > 5 seconds
            let jump_detected = (position_delta_secs.abs() - elapsed_secs).abs()
                > JUMP_THRESHOLD_SECS
                && elapsed_secs > 0.1;

            if jump_detected
                && position_ticks <= session.max_intro_duration_ticks
                && !session.is_paused
            {
                session.last_jump_position_ticks = Some(position_ticks);

                if session.first_jump_position_ticks.is_none() {
                    let started_from_beginning = session.playback_start_ticks < 5 * TICKS_PER_SEC;
                    let is_forward = position_delta_ticks > 0;
                    let jump_origin = session.previous_position_ticks;

                    if started_from_beginning && is_forward {
                        session.first_jump_position_ticks = Some(session.previous_position_ticks);
                        if jump_origin > session.min_opening_plot_duration_ticks {
                            session.max_intro_duration_ticks = jump_origin;
                        }
                    }
                }
            }

            // Check if we should write intro markers
            let mut intro_action = None;
            if position_ticks >= session.max_intro_duration_ticks && session.intro_end.is_none() {
                if let Some(intro_end) = session.last_jump_position_ticks {
                    let intro_start = session
                        .first_jump_position_ticks
                        .unwrap_or(0)
                        .min(intro_end);

                    if intro_start < intro_end {
                        intro_action = Some((session.item_id.clone(), intro_start, intro_end));
                    }
                }
            }

            // Pause/resume detection for credits
            let mut credits_action = None;
            if is_paused && !session.is_paused {
                session.last_pause_event_time = Some(now);
                session.is_paused = true;
            } else if !is_paused && session.is_paused {
                if let Some(pause_time) = session.last_pause_event_time {
                    let pause_duration = now.duration_since(pause_time);
                    let pause_ms = pause_duration.as_millis() as u64;

                    if (PAUSE_MIN_DURATION_MS..=PAUSE_MAX_DURATION_MS).contains(&pause_ms) {
                        let credits_window =
                            session.runtime_ticks - session.max_credits_duration_ticks;
                        if position_ticks > credits_window && session.credits_start.is_none() {
                            credits_action = Some((session.item_id.clone(), position_ticks));
                        }
                    }
                }
                session.is_paused = false;
                session.last_pause_event_time = None;
            }

            // Update tracking state
            session.previous_position_ticks = position_ticks;
            session.previous_event_time = now;

            (session.item_id.clone(), intro_action, credits_action, true)
        };

        // Execute actions outside the borrow
        if let Some((item_id, intro_start, intro_end)) = intro_action {
            self.try_update_intro(&item_id, intro_start, intro_end);
        }
        if let Some((item_id, credits_start)) = credits_action {
            self.try_update_credits(&item_id, credits_start);
        }
        let _ = (_item_id, _update_state);
    }

    /// Called when playback stops.
    pub fn on_playback_stopped(&self, play_session_id: &str, position_ticks: i64) {
        if let Some(session) = self.sessions.get(play_session_id) {
            // Credits detection on stop
            let credits_window = session.runtime_ticks - session.max_credits_duration_ticks;
            if position_ticks > credits_window && session.credits_start.is_none() {
                let item_id = session.item_id.clone();
                drop(session);
                self.try_update_credits(&item_id, position_ticks);
            }
        }
        self.sessions.remove(play_session_id);
    }

    fn try_update_intro(&self, item_id: &str, intro_start: i64, intro_end: i64) {
        let now = Instant::now();
        if let Some(last) = self.last_intro_update.get(item_id) {
            if now.duration_since(*last) < Duration::from_secs(UPDATE_COOLDOWN_SECS) {
                return;
            }
        }
        self.last_intro_update.insert(item_id.to_string(), now);

        // Spawn background task to write markers
        let item_id = item_id.to_string();
        tokio::spawn(async move {
            // TODO: Call chapters::update_intro_for_season when chapters module is ready
            tracing::info!("Intro detected for {item_id}: start={intro_start}, end={intro_end}");
        });
    }

    fn try_update_credits(&self, item_id: &str, credits_start: i64) {
        let now = Instant::now();
        if let Some(last) = self.last_credits_update.get(item_id) {
            if now.duration_since(*last) < Duration::from_secs(UPDATE_COOLDOWN_SECS) {
                return;
            }
        }
        self.last_credits_update.insert(item_id.to_string(), now);

        // Spawn background task to write markers
        let item_id = item_id.to_string();
        tokio::spawn(async move {
            // TODO: Call chapters::update_credits when chapters module is ready
            tracing::info!("Credits detected for {item_id}: start={credits_start}");
        });
    }
}

impl Default for IntroDetector {
    fn default() -> Self {
        Self::new(150, 360, 60)
    }
}
