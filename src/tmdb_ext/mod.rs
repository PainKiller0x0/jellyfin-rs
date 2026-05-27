use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lru::LruCache;
use reqwest::Client;
use serde::de::DeserializeOwned;

/// Rate-limited TMDB API client with LRU cache.
pub struct TmdbClient {
    client: Client,
    api_key: String,
    rate_limit_ms: u64,
    last_request: Mutex<Instant>,
    cache: Mutex<LruCache<String, String>>,
}

impl TmdbClient {
    pub fn new(client: Client, api_key: String, rate_limit_ms: u64, cache_size: usize) -> Self {
        Self {
            client,
            api_key,
            rate_limit_ms,
            last_request: Mutex::new(Instant::now() - Duration::from_millis(1000)),
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(cache_size.max(1)).unwrap(),
            )),
        }
    }

    /// Make a rate-limited GET request to the TMDB API with caching.
    pub async fn get<T: DeserializeOwned>(&self, endpoint: &str) -> anyhow::Result<T> {
        let url = format!("https://api.themoviedb.org/3/{endpoint}");

        // Check cache
        {
            let mut cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(&url) {
                return Ok(serde_json::from_str(cached)?);
            }
        }

        // Rate limiting
        {
            let mut last = self.last_request.lock().unwrap();
            let elapsed = last.elapsed();
            let min_interval = Duration::from_millis(self.rate_limit_ms);
            if elapsed < min_interval {
                std::thread::sleep(min_interval - elapsed);
            }
            *last = Instant::now();
        }

        let response = self
            .client
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .header("Accept", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("TMDB API error {status}: {body}");
        }

        let body = response.text().await?;

        // Update cache
        {
            let mut cache = self.cache.lock().unwrap();
            cache.put(url, body.clone());
        }

        Ok(serde_json::from_str(&body)?)
    }

    /// Get person details from TMDB.
    pub async fn get_person(&self, person_id: &str, language: &str) -> anyhow::Result<serde_json::Value> {
        self.get(&format!("person/{person_id}?language={language}"))
            .await
    }

    /// Get TV show details from TMDB.
    pub async fn get_tv_show(&self, show_id: &str, language: &str) -> anyhow::Result<serde_json::Value> {
        self.get(&format!("tv/{show_id}?language={language}"))
            .await
    }

    /// Get episode details from TMDB.
    pub async fn get_episode(
        &self,
        show_id: &str,
        season: i64,
        episode: i64,
        language: &str,
    ) -> anyhow::Result<serde_json::Value> {
        self.get(&format!(
            "tv/{show_id}/season/{season}/episode/{episode}?language={language}"
        ))
        .await
    }

    /// Get movie details from TMDB.
    pub async fn get_movie(&self, movie_id: &str, language: &str) -> anyhow::Result<serde_json::Value> {
        self.get(&format!("movie/{movie_id}?language={language}"))
            .await
    }
}
