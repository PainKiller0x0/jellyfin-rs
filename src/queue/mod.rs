use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

/// Two-tier concurrent processing pipeline.
/// Master semaphore controls heavy operations (probe, mediainfo extract).
/// Tier2 semaphore controls lighter operations (TMDb refresh, subtitle scan).
pub struct QueueManager {
    pub master_semaphore: Arc<Semaphore>,
    pub tier2_semaphore: Arc<Semaphore>,
    mediainfo_tx: mpsc::UnboundedSender<String>,
    fingerprint_tx: mpsc::UnboundedSender<String>,
    intro_skip_tx: mpsc::UnboundedSender<String>,
    episode_refresh_tx: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
}

impl QueueManager {
    pub fn new(master_concurrency: usize, tier2_concurrency: usize) -> Self {
        let (mediainfo_tx, _) = mpsc::unbounded_channel();
        let (fingerprint_tx, _) = mpsc::unbounded_channel();
        let (intro_skip_tx, _) = mpsc::unbounded_channel();
        let (episode_refresh_tx, _) = mpsc::unbounded_channel();

        Self {
            master_semaphore: Arc::new(Semaphore::new(master_concurrency)),
            tier2_semaphore: Arc::new(Semaphore::new(tier2_concurrency)),
            mediainfo_tx,
            fingerprint_tx,
            intro_skip_tx,
            episode_refresh_tx,
            cancel: CancellationToken::new(),
        }
    }

    pub fn mediainfo_sender(&self) -> mpsc::UnboundedSender<String> {
        self.mediainfo_tx.clone()
    }

    pub fn fingerprint_sender(&self) -> mpsc::UnboundedSender<String> {
        self.fingerprint_tx.clone()
    }

    pub fn intro_skip_sender(&self) -> mpsc::UnboundedSender<String> {
        self.intro_skip_tx.clone()
    }

    pub fn episode_refresh_sender(&self) -> mpsc::UnboundedSender<String> {
        self.episode_refresh_tx.clone()
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Default for QueueManager {
    fn default() -> Self {
        Self::new(1, 1)
    }
}
