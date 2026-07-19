use std::{path::Path, time::Duration};

use anyhow::Context;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, COOKIE, REFERER, USER_AGENT};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    db::row_ext::QueryResultExt,
    util::{normalize_yyyy_mm_dd, year_from_yyyy_mm_dd},
};

const DOUBAN_PROVIDER: &str = "Douban";
const SEARCH_URL: &str = "https://movie.douban.com/subject_search";
const SUGGEST_URL: &str = "https://movie.douban.com/j/subject_suggest";
const DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148";

#[derive(Clone, Debug, Default)]
pub struct DoubanSubject {
    pub id: String,
    pub title: String,
    pub item_type: Option<String>,
    pub year: Option<i64>,
    pub premiere_date: Option<String>,
    pub overview: Option<String>,
    pub image_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub community_rating: Option<f64>,
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub runtime_ticks: Option<i64>,
    pub episode_count: Option<i64>,
    pub people: Vec<Value>,
}

impl DoubanSubject {
    pub fn to_remote_value(&self, fallback_type: &str) -> Value {
        let item_type = self
            .item_type
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback_type);
        let mut value = json!({
            "Name": self.title,
            "Type": item_type,
            "ProductionYear": self.year,
            "PremiereDate": self.premiere_date,
            "SearchProviderName": DOUBAN_PROVIDER,
            "ProviderIds": { DOUBAN_PROVIDER: self.id },
            "ImageUrl": self.image_url,
            "Overview": self.overview,
            "CommunityRating": self.community_rating,
        });
        if !self.genres.is_empty() {
            value["Genres"] = json!(self.genres);
        }
        if !self.countries.is_empty() {
            value["Countries"] = json!(self.countries);
        }
        if let Some(runtime_ticks) = self.runtime_ticks {
            value["RuntimeTicks"] = json!(runtime_ticks);
        }
        if let Some(episode_count) = self.episode_count {
            value["EpisodeCount"] = json!(episode_count);
        }
        if !self.people.is_empty() {
            value["People"] = Value::Array(self.people.clone());
        }
        if let Some(backdrop_url) = self
            .backdrop_url
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            value["BackdropUrl"] = json!(backdrop_url);
        }
        value
    }
}

#[derive(Deserialize)]
struct DoubanSearchPage {
    #[serde(default)]
    items: Vec<DoubanSearchItem>,
}

#[derive(Deserialize)]
struct DoubanSearchItem {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    title: String,
    #[serde(default)]
    abstract_2: Option<String>,
    #[serde(default, rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(default)]
    cover_url: Option<String>,
    #[serde(default)]
    labels: Vec<DoubanSearchLabel>,
    #[serde(default)]
    rating: Option<DoubanSearchRating>,
}

#[derive(Deserialize)]
struct DoubanSearchLabel {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct DoubanSearchRating {
    #[serde(default)]
    value: Option<f64>,
}

#[derive(Deserialize)]
struct DoubanSuggestItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    sub_title: Option<String>,
    #[serde(default)]
    img: Option<String>,
    #[serde(default)]
    year: Option<String>,
    #[serde(default)]
    episode: Option<String>,
}

pub async fn douban_search(
    client: &reqwest::Client,
    cookie: Option<&str>,
    name: &str,
    year: Option<i64>,
    item_type: &str,
) -> anyhow::Result<Vec<Value>> {
    let subjects = search_subjects(client, cookie, name, year, item_type).await?;
    Ok(subjects
        .into_iter()
        .take(20)
        .map(|subject| subject.to_remote_value(item_type))
        .collect())
}

pub async fn douban_details(
    client: &reqwest::Client,
    cookie: Option<&str>,
    douban_id: &str,
    fallback_type: &str,
) -> anyhow::Result<Value> {
    let subject = fetch_subject_details(client, cookie, douban_id, fallback_type).await?;
    Ok(subject.to_remote_value(fallback_type))
}

pub async fn fill_missing_douban(
    db: &DatabaseConnection,
    cookie: Option<&str>,
) -> anyhow::Result<usize> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            r#"SELECT mi.id, mi.title, mi.path, mi.item_type, p.provider_item_id AS douban_id
           FROM media_items mi
           LEFT JOIN provider_ids p ON p.item_id = mi.id AND p.provider = 'Douban'
           WHERE mi.is_folder = 1
             AND mi.item_type IN ('Movie', 'Series')
             AND (
                 p.provider_item_id IS NULL
                 OR mi.overview IS NULL
                 OR mi.production_year IS NULL
                 OR mi.premiere_date IS NULL
                 OR NOT EXISTS (
                     SELECT 1 FROM image_assets ia
                     WHERE ia.item_id = mi.id AND ia.image_type = 'Primary'
                 )
             )"#,
            vec![],
        ))
        .await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let client = crate::util::http_client().context("failed to build Douban HTTP client")?;
    let total = rows.len();
    let mut filled = 0usize;
    tracing::info!("fill_missing_douban: {total} items need name-based Douban lookup");

    for row in rows {
        let item_id = match row.get_str("id") {
            Ok(value) => value,
            Err(_) => continue,
        };
        let title = row.get_str("title").unwrap_or_default();
        let item_type = row.get_str("item_type").unwrap_or_default();
        let path = row.get_str("path").unwrap_or_default();
        if let Ok(Some(douban_id)) = row.get_opt_str("douban_id") {
            match fetch_subject_details(&client, cookie, &douban_id, &item_type).await {
                Ok(details) => {
                    apply_subject_metadata(db, &item_id, &details, cookie).await?;
                    filled += 1;
                    tracing::info!(
                        "fill_missing_douban: refreshed '{}' from douban-{}",
                        title,
                        details.id
                    );
                    tokio::time::sleep(Duration::from_millis(350)).await;
                }
                Err(error) => {
                    tracing::warn!(
                        "fill_missing_douban: detail fetch failed for stored id {douban_id}: {error:#}"
                    );
                }
            }
            continue;
        }

        let (name, year) =
            parse_lookup_name(Path::new(&path)).unwrap_or_else(|| clean_title_year(&title));
        if should_skip_douban_lookup(&name) {
            tracing::debug!("fill_missing_douban: skipped generic folder name '{name}'");
            continue;
        }

        match search_subjects(&client, cookie, &name, year, &item_type).await {
            Ok(subjects) => {
                let Some(best) = subjects.into_iter().next() else {
                    tracing::warn!(
                        "fill_missing_douban: no match for '{name}' (type: {item_type})"
                    );
                    continue;
                };
                let details =
                    match fetch_subject_details(&client, cookie, &best.id, &item_type).await {
                        Ok(details) => merge_subject(best, details),
                        Err(error) => {
                            tracing::warn!(
                                "fill_missing_douban: detail fetch failed for {}: {error:#}",
                                best.id
                            );
                            best
                        }
                    };
                apply_subject_metadata(db, &item_id, &details, cookie).await?;
                filled += 1;
                tracing::info!(
                    "fill_missing_douban: matched '{name}' -> douban-{}",
                    details.id
                );
                tokio::time::sleep(Duration::from_millis(350)).await;
            }
            Err(error) => {
                tracing::warn!("fill_missing_douban: search failed for '{name}': {error:#}");
            }
        }
    }

    tracing::info!("fill_missing_douban: filled {filled}/{total} items");
    Ok(filled)
}

async fn search_subjects(
    client: &reqwest::Client,
    cookie: Option<&str>,
    name: &str,
    year: Option<i64>,
    item_type: &str,
) -> anyhow::Result<Vec<DoubanSubject>> {
    let mut request = douban_request(client.get(SEARCH_URL), cookie, false)
        .query(&[("search_text", name), ("cat", "1002")]);
    if let Some(year) = year {
        let year_string = year.to_string();
        request = request.query(&[("year", year_string.as_str())]);
    }

    let html = request.send().await?.error_for_status()?.text().await?;
    let mut subjects = if let Some(data) = extract_window_data(&html) {
        parse_search_data(data)?
    } else {
        Vec::new()
    };

    if subjects.is_empty() {
        subjects = suggest_subjects(client, cookie, name, item_type).await?;
    }

    if subjects.is_empty() {
        return Ok(Vec::new());
    }

    let expected = normalize_item_type(item_type);
    let mut filtered = subjects
        .iter()
        .filter(|subject| {
            subject
                .item_type
                .as_deref()
                .map(normalize_item_type)
                .is_none_or(|actual| actual == expected)
        })
        .cloned()
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        filtered = subjects;
    }

    filtered.sort_by(|left, right| {
        score_subject(right, name, year, &expected).cmp(&score_subject(left, name, year, &expected))
    });
    Ok(filtered)
}

async fn suggest_subjects(
    client: &reqwest::Client,
    cookie: Option<&str>,
    name: &str,
    item_type: &str,
) -> anyhow::Result<Vec<DoubanSubject>> {
    let items = douban_request(client.get(SUGGEST_URL), cookie, false)
        .header(ACCEPT, "application/json,text/javascript,*/*;q=0.8")
        .header("X-Requested-With", "XMLHttpRequest")
        .query(&[("q", name)])
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<DoubanSuggestItem>>()
        .await?;
    Ok(items
        .into_iter()
        .filter_map(|item| {
            if item.id.trim().is_empty() {
                return None;
            }
            let (title, title_year) =
                clean_title_year(item.sub_title.as_deref().unwrap_or(&item.title));
            let year = item
                .year
                .as_deref()
                .and_then(|value| value.trim().parse::<i64>().ok())
                .or(title_year);
            Some(DoubanSubject {
                id: item.id.trim().to_string(),
                title,
                item_type: Some(item_type.to_string()),
                year,
                image_url: item.img.map(clean_image_url),
                episode_count: item
                    .episode
                    .as_deref()
                    .and_then(|value| value.trim().parse::<i64>().ok()),
                ..Default::default()
            })
        })
        .collect())
}

fn parse_search_data(data: &str) -> anyhow::Result<Vec<DoubanSubject>> {
    let page: DoubanSearchPage =
        serde_json::from_str(data).context("invalid Douban search JSON")?;
    Ok(page
        .items
        .into_iter()
        .filter_map(|item| {
            let id = value_id_string(&item.id)?;
            let (title, title_year) = clean_title_year(&item.title);
            if title.is_empty() {
                return None;
            }
            let facts = parse_fact_text(item.abstract_text.as_deref().unwrap_or_default());
            let people = parse_people_text(item.abstract_2.as_deref().unwrap_or_default());
            let inferred_type = infer_search_item_type(&item.labels, item.abstract_text.as_deref());
            Some(DoubanSubject {
                id,
                title,
                item_type: inferred_type,
                year: title_year.or(facts.year).or_else(|| {
                    facts
                        .premiere_date
                        .as_deref()
                        .and_then(year_from_yyyy_mm_dd)
                }),
                premiere_date: facts.premiere_date,
                overview: item.abstract_text.as_deref().map(clean_text),
                image_url: item.cover_url.map(clean_image_url),
                community_rating: item.rating.and_then(|rating| rating.value),
                genres: facts.genres,
                countries: facts.countries,
                runtime_ticks: facts.runtime_ticks,
                episode_count: facts.episode_count,
                people,
                ..Default::default()
            })
        })
        .collect())
}

async fn fetch_subject_details(
    client: &reqwest::Client,
    cookie: Option<&str>,
    douban_id: &str,
    fallback_type: &str,
) -> anyhow::Result<DoubanSubject> {
    let url = format!("https://m.douban.com/movie/subject/{douban_id}/");
    let html = douban_request(client.get(url), cookie, true)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let parsed = parse_mobile_subject_html(douban_id, &html, fallback_type);
    if parsed.title.is_empty() {
        anyhow::bail!("Douban detail page did not contain subject metadata");
    }
    Ok(parsed)
}

fn parse_mobile_subject_html(douban_id: &str, html: &str, fallback_type: &str) -> DoubanSubject {
    let raw_title = meta_content(html, "itemprop=\"name\"")
        .or_else(|| meta_content(html, "property=\"og:title\""))
        .or_else(|| text_between(html, "<div class=\"sub-title\">", "</div>").map(clean_text))
        .unwrap_or_default();
    let (title, title_year) = clean_title_year(&raw_title);
    let detail_type =
        infer_detail_item_type(&raw_title).or_else(|| Some(fallback_type.to_string()));
    let overview = text_between_class_section(html, "subject-intro")
        .map(|value| clean_intro_text(&value))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            meta_content(html, "itemprop=\"description\"")
                .or_else(|| meta_content(html, "property=\"og:description\""))
                .map(|value| clean_meta_description(&value))
                .filter(|value| !value.is_empty())
        });
    let image_url = meta_content(html, "itemprop=\"image\"")
        .or_else(|| meta_content(html, "property=\"og:image\""))
        .or_else(|| first_tag_attr_after(html, "class=\"sub-cover\"", "src"))
        .map(clean_image_url);
    let facts = text_between(html, "<div class=\"sub-meta\">", "</div>")
        .map(|value| parse_fact_text(&strip_tags(&value)))
        .unwrap_or_default();
    let backdrop_url = first_subject_pic_url(html)
        .map(clean_image_url)
        .filter(|value| image_url.as_ref() != Some(value));

    DoubanSubject {
        id: douban_id.trim().to_string(),
        title,
        item_type: detail_type,
        year: title_year.or(facts.year).or_else(|| {
            facts
                .premiere_date
                .as_deref()
                .and_then(year_from_yyyy_mm_dd)
        }),
        premiere_date: facts.premiere_date,
        overview,
        image_url,
        backdrop_url,
        community_rating: meta_content(html, "itemprop=\"ratingValue\"")
            .and_then(|value| value.trim().parse::<f64>().ok()),
        genres: facts.genres,
        countries: facts.countries,
        runtime_ticks: facts.runtime_ticks,
        episode_count: facts.episode_count,
        ..Default::default()
    }
}

async fn apply_subject_metadata(
    db: &DatabaseConnection,
    item_id: &str,
    subject: &DoubanSubject,
    cookie: Option<&str>,
) -> anyhow::Result<()> {
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO provider_ids (item_id, provider, provider_item_id)
           VALUES (?, 'Douban', ?)
           ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id"#,
        vec![item_id.into(), subject.id.as_str().into()],
    ))
    .await
    .with_context(|| format!("failed to save Douban provider id for item: {item_id}"))?;

    db.execute(crate::db::helpers::pg_statement(
        r#"UPDATE media_items
           SET overview = COALESCE(overview, ?),
               production_year = COALESCE(production_year, ?),
               premiere_date = COALESCE(premiere_date, ?),
               community_rating = COALESCE(community_rating, ?),
               runtime_ticks = COALESCE(runtime_ticks, ?),
               updated_at = ?
           WHERE id = ?"#,
        vec![
            subject.overview.as_deref().into(),
            subject.year.into(),
            subject.premiere_date.as_deref().into(),
            subject.community_rating.into(),
            subject.runtime_ticks.into(),
            crate::util::now_unix().into(),
            item_id.into(),
        ],
    ))
    .await
    .with_context(|| format!("failed to apply Douban metadata for item: {item_id}"))?;

    upsert_named_relations(
        db,
        item_id,
        "genres",
        "media_genres",
        "genre_id",
        &subject.genres,
    )
    .await?;
    upsert_people(db, item_id, &subject.people).await?;

    let client = crate::util::http_client().context("failed to build Douban image client")?;
    if let Some(image_url) = subject
        .image_url
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let _ = download_and_save_douban_image(db, &client, item_id, image_url, "Primary", cookie)
            .await;
    }
    if let Some(backdrop_url) = subject
        .backdrop_url
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let _ =
            download_and_save_douban_image(db, &client, item_id, backdrop_url, "Backdrop", cookie)
                .await;
        let _ =
            download_and_save_douban_image(db, &client, item_id, backdrop_url, "Art", cookie).await;
    }
    Ok(())
}

async fn upsert_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    names: &[String],
) -> anyhow::Result<()> {
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let id = upsert_name_get_id(db, table, name, &format!("{table}:")).await?;
        db.execute(crate::db::helpers::pg_statement(
            &format!("INSERT INTO {relation_table} (item_id, {relation_column}) VALUES (?, ?) ON CONFLICT(item_id, {relation_column}) DO NOTHING"),
            vec![item_id.into(), id.into()],
        ))
        .await?;
    }
    Ok(())
}

async fn upsert_people(
    db: &DatabaseConnection,
    item_id: &str,
    people: &[Value],
) -> anyhow::Result<()> {
    for (sort_order, person) in people.iter().enumerate() {
        let Some(name) = person
            .get("Name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let person_type = person
            .get("Type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("Actor");
        let role = person
            .get("Role")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let id = upsert_name_get_id(db, "people", name, "people:").await?;
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET role = COALESCE(excluded.role, media_people.role), sort_order = excluded.sort_order",
            vec![
                item_id.into(),
                id.into(),
                role.into(),
                person_type.into(),
                i64::try_from(sort_order).unwrap_or(i64::MAX).into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

async fn upsert_name_get_id(
    db: &DatabaseConnection,
    table: &str,
    name: &str,
    id_prefix: &str,
) -> anyhow::Result<String> {
    let id = crate::util::stable_text_id(&format!("{id_prefix}{}", name.to_ascii_lowercase()));
    db.execute(crate::db::helpers::pg_statement(
        &format!(
            "INSERT INTO {table} (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING"
        ),
        vec![id.clone().into(), name.into(), crate::util::now_unix().into()],
    ))
    .await?;
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            &format!("SELECT id FROM {table} WHERE name = ?"),
            vec![name.into()],
        ))
        .await?;
    Ok(row.and_then(|row| row.get_str("id").ok()).unwrap_or(id))
}

async fn download_and_save_douban_image(
    db: &DatabaseConnection,
    client: &reqwest::Client,
    item_id: &str,
    url: &str,
    image_type: &str,
    cookie: Option<&str>,
) -> anyhow::Result<()> {
    let response = douban_request(client.get(url), cookie, true)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;
    let ext = image_extension(url);
    let dir = std::path::PathBuf::from("data").join("images");
    tokio::fs::create_dir_all(&dir).await.ok();
    let path = dir.join(format!(
        "{}_{}_douban.{}",
        crate::util::stable_text_id(item_id),
        image_type.to_ascii_lowercase(),
        ext
    ));
    tokio::fs::write(&path, &bytes).await?;
    let now = crate::util::now_unix();
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, width, height, size_bytes, created_at, updated_at)
           VALUES (?, ?, ?, 0, ?, ?, NULL, NULL, ?, ?, ?)
           ON CONFLICT(item_id, image_type, image_index)
           DO UPDATE SET path = excluded.path, etag = excluded.etag, size_bytes = excluded.size_bytes, updated_at = excluded.updated_at"#,
        vec![
            crate::util::stable_text_id(&format!("image-asset:{item_id}:{image_type}:0")).into(),
            item_id.into(),
            image_type.into(),
            path.to_string_lossy().to_string().into(),
            crate::util::stable_text_id(&format!("douban:{item_id}:{image_type}:{url}")).into(),
            i64::try_from(bytes.len()).unwrap_or(i64::MAX).into(),
            now.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

fn douban_request(
    request: reqwest::RequestBuilder,
    cookie: Option<&str>,
    mobile: bool,
) -> reqwest::RequestBuilder {
    let mut request = request
        .header(
            USER_AGENT,
            if mobile {
                MOBILE_USER_AGENT
            } else {
                DESKTOP_USER_AGENT
            },
        )
        .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.6")
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(REFERER, "https://movie.douban.com/");
    if let Some(cookie) = cookie.map(str::trim).filter(|value| !value.is_empty()) {
        let cookie = crate::app::state::normalize_douban_cookie_value(cookie);
        if !cookie.is_empty() {
            request = request.header(COOKIE, cookie);
        }
    }
    request
}

fn merge_subject(mut base: DoubanSubject, details: DoubanSubject) -> DoubanSubject {
    if !details.title.is_empty() {
        base.title = details.title;
    }
    base.item_type = details.item_type.or(base.item_type);
    base.year = details.year.or(base.year);
    base.premiere_date = details.premiere_date.or(base.premiere_date);
    base.overview = details.overview.or(base.overview);
    base.image_url = details.image_url.or(base.image_url);
    base.backdrop_url = details.backdrop_url.or(base.backdrop_url);
    base.community_rating = details.community_rating.or(base.community_rating);
    if !details.genres.is_empty() {
        base.genres = details.genres;
    }
    if !details.countries.is_empty() {
        base.countries = details.countries;
    }
    base.runtime_ticks = details.runtime_ticks.or(base.runtime_ticks);
    base.episode_count = details.episode_count.or(base.episode_count);
    if !details.people.is_empty() {
        base.people = details.people;
    }
    base
}

fn score_subject(subject: &DoubanSubject, query: &str, year: Option<i64>, expected: &str) -> i64 {
    let folded_title = fold_lookup_name(&subject.title);
    let folded_query = fold_lookup_name(query);
    let mut score = 0;
    if folded_title == folded_query {
        score += 100;
    } else if folded_title.contains(&folded_query) || folded_query.contains(&folded_title) {
        score += 40;
    }
    if subject.year.is_some() && subject.year == year {
        score += 30;
    }
    if subject
        .item_type
        .as_deref()
        .map(normalize_item_type)
        .is_some_and(|actual| actual == expected)
    {
        score += 20;
    }
    if subject.community_rating.unwrap_or_default() > 0.0 {
        score += 5;
    }
    score
}

fn parse_lookup_name(path: &Path) -> Option<(String, Option<i64>)> {
    let name = path.file_name()?.to_str()?;
    let cleaned = remove_provider_tags(name);
    let (title, year) = clean_title_year(&cleaned);
    (!title.is_empty()).then_some((title, year))
}

fn remove_provider_tags(value: &str) -> String {
    let mut result = value.to_string();
    for (open, close) in [('{', '}'), ('[', ']')] {
        while let (Some(start), Some(end)) = (result.rfind(open), result.rfind(close)) {
            if start >= end {
                break;
            }
            let tag = result[start + 1..end].to_ascii_lowercase();
            if tag.starts_with("tmdb-")
                || tag.starts_with("tmdbid=")
                || tag.starts_with("douban-")
                || tag.starts_with("doubanid=")
            {
                result.replace_range(start..=end, "");
            } else {
                break;
            }
        }
    }
    result
}

fn should_skip_douban_lookup(name: &str) -> bool {
    let folded = fold_lookup_name(name);
    folded.is_empty()
        || matches!(
            folded.as_str(),
            "media" | "movie" | "movies" | "film" | "films" | "tv" | "series" | "season"
        )
        || folded
            .strip_prefix("season ")
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
}

fn infer_search_item_type(
    labels: &[DoubanSearchLabel],
    abstract_text: Option<&str>,
) -> Option<String> {
    if labels.iter().any(|label| label.text.contains("剧集")) {
        return Some("Series".to_string());
    }
    if labels.iter().any(|label| label.text.contains("电影")) {
        return Some("Movie".to_string());
    }
    let abstract_text = abstract_text.unwrap_or_default();
    if abstract_text.contains('集') {
        Some("Series".to_string())
    } else {
        None
    }
}

fn infer_detail_item_type(title: &str) -> Option<String> {
    if title.contains("电视剧") || title.contains("剧集") {
        Some("Series".to_string())
    } else if title.contains("电影") {
        Some("Movie".to_string())
    } else {
        None
    }
}

fn normalize_item_type(value: &str) -> String {
    if value.eq_ignore_ascii_case("Series")
        || value.eq_ignore_ascii_case("Season")
        || value.eq_ignore_ascii_case("Episode")
    {
        "Series".to_string()
    } else if value.eq_ignore_ascii_case("Movie") {
        "Movie".to_string()
    } else {
        value.to_string()
    }
}

#[derive(Default)]
struct ParsedFacts {
    year: Option<i64>,
    premiere_date: Option<String>,
    genres: Vec<String>,
    countries: Vec<String>,
    runtime_ticks: Option<i64>,
    episode_count: Option<i64>,
}

fn parse_fact_text(value: &str) -> ParsedFacts {
    let mut facts = ParsedFacts::default();
    for part in value
        .split('/')
        .map(clean_text)
        .filter(|part| !part.is_empty())
    {
        if facts.year.is_none() {
            facts.year = year_from_text(&part);
        }
        if facts.premiere_date.is_none() {
            facts.premiere_date = normalize_yyyy_mm_dd(&part);
        }
        if facts.runtime_ticks.is_none() {
            facts.runtime_ticks = runtime_minutes_from_text(&part).map(minutes_to_ticks);
        }
        if facts.episode_count.is_none() {
            facts.episode_count = episode_count_from_text(&part);
        }
        if is_country_part(&part) {
            push_unique(&mut facts.countries, part);
            continue;
        }
        if is_genre_part(&part) {
            push_unique(&mut facts.genres, part);
        }
    }
    facts
}

fn is_country_part(value: &str) -> bool {
    const COUNTRIES: &[&str] = &[
        "中国大陆",
        "中国香港",
        "中国台湾",
        "中国澳门",
        "美国",
        "英国",
        "日本",
        "韩国",
        "法国",
        "德国",
        "意大利",
        "西班牙",
        "加拿大",
        "澳大利亚",
        "印度",
        "泰国",
        "俄罗斯",
        "中国",
    ];
    COUNTRIES.contains(&value)
}

fn is_genre_part(value: &str) -> bool {
    if value.chars().count() > 16 {
        return false;
    }
    !value.contains("上映")
        && !value.contains("分钟")
        && !value.contains('集')
        && !value.contains('-')
        && !value.chars().any(|ch| ch.is_ascii_digit())
}

fn parse_people_text(value: &str) -> Vec<Value> {
    value
        .split('/')
        .map(clean_text)
        .filter(|name| !name.is_empty() && name.chars().count() <= 64)
        .take(20)
        .map(|name| json!({ "Name": name, "Type": "Actor" }))
        .collect()
}

fn clean_title_year(value: &str) -> (String, Option<i64>) {
    let mut title = clean_text(value)
        .replace('\u{200e}', "")
        .replace('\u{200f}', "");
    for suffix in [" - 电视剧", " - 电影", "- 电视剧", "- 电影"] {
        if let Some(stripped) = title.strip_suffix(suffix) {
            title = stripped.trim().to_string();
        }
    }
    let mut year = None;
    if let (Some(start), Some(end)) = (title.rfind('('), title.rfind(')')) {
        if start < end && end == title.len() - 1 {
            let inner = title[start + 1..end].trim();
            if let Ok(parsed) = inner.parse::<i64>() {
                if (1880..=2100).contains(&parsed) {
                    year = Some(parsed);
                    title.replace_range(start..=end, "");
                }
            }
        }
    }
    if year.is_none() {
        year = year_from_text(&title);
    }
    (title.trim().to_string(), year)
}

fn clean_intro_text(value: &str) -> String {
    let mut text = strip_tags(value);
    for prefix in ["剧集简介", "电影简介", "剧情简介", "简介"] {
        if let Some(stripped) = text.strip_prefix(prefix) {
            text = stripped.trim().to_string();
        }
    }
    text
}

fn clean_meta_description(value: &str) -> String {
    let text = clean_text(value);
    if let Some((_, overview)) = text.split_once("简介：") {
        return overview.trim().to_string();
    }
    if let Some((_, overview)) = text.split_once("简介:") {
        return overview.trim().to_string();
    }
    text
}

fn clean_image_url(value: String) -> String {
    decode_html_entities(value.split(['?', '#']).next().unwrap_or(&value))
        .trim()
        .to_string()
}

fn text_between<'a>(value: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_at = value.find(start)? + start.len();
    let tail = &value[start_at..];
    let end_at = tail.find(end)?;
    Some(&tail[..end_at])
}

fn text_between_class_section(html: &str, class_name: &str) -> Option<String> {
    let class_marker = format!("class=\"{class_name}\"");
    let class_at = html.find(&class_marker)?;
    let start = html[..class_at].rfind("<section")?;
    let tail = &html[start..];
    let end = tail.find("</section>")?;
    Some(tail[..end].to_string())
}

fn first_tag_attr_after(html: &str, marker: &str, attr: &str) -> Option<String> {
    let pos = html.find(marker)?;
    let tail = &html[pos..];
    let tag_end = tail.find('>')?;
    attr_value(&tail[..tag_end], attr)
}

fn first_subject_pic_url(html: &str) -> Option<String> {
    let section_pos = html.find("subject-pics")?;
    let section = &html[section_pos..];
    first_tag_attr_after(section, "data-src=", "data-src")
        .or_else(|| first_tag_attr_after(section, "src=", "src"))
}

fn meta_content(html: &str, marker: &str) -> Option<String> {
    let marker_at = html.find(marker)?;
    let tag_start = html[..marker_at].rfind('<')?;
    let tag_end = html[marker_at..].find('>')? + marker_at;
    attr_value(&html[tag_start..=tag_end], "content").map(|value| clean_text(&value))
}

fn attr_value(tag: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let marker = format!("{attr}={quote}");
        let start = tag.find(&marker)? + marker.len();
        let tail = &tag[start..];
        let end = tail.find(quote)?;
        return Some(decode_html_entities(&tail[..end]));
    }
    None
}

fn strip_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    clean_text(&output)
}

fn clean_text(value: &str) -> String {
    decode_html_entities(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn decode_html_entities(value: &str) -> String {
    let mut out = value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");
    while let Some(start) = out.find("&#x") {
        let Some(end) = out[start + 3..].find(';').map(|end| start + 3 + end) else {
            break;
        };
        let hex = &out[start + 3..end];
        let Some(ch) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) else {
            break;
        };
        out.replace_range(start..=end, &ch.to_string());
    }
    while let Some(start) = out.find("&#") {
        let Some(end) = out[start + 2..].find(';').map(|end| start + 2 + end) else {
            break;
        };
        let dec = &out[start + 2..end];
        let Some(ch) = dec.parse::<u32>().ok().and_then(char::from_u32) else {
            break;
        };
        out.replace_range(start..=end, &ch.to_string());
    }
    out
}

fn extract_window_data(html: &str) -> Option<&str> {
    let marker_at = html.find("window.__DATA__")?;
    let tail = &html[marker_at..];
    let brace_at = tail.find('{')?;
    extract_balanced_json_object(&tail[brace_at..])
}

fn extract_balanced_json_object(value: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&value[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn value_id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().to_string()).filter(|value| !value.is_empty()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn year_from_text(value: &str) -> Option<i64> {
    for window in value.as_bytes().windows(4) {
        let Ok(digits) = std::str::from_utf8(window) else {
            continue;
        };
        let Ok(year) = digits.parse::<i64>() else {
            continue;
        };
        if (1880..=2100).contains(&year) {
            return Some(year);
        }
    }
    None
}

fn runtime_minutes_from_text(value: &str) -> Option<i64> {
    if !value.contains("分钟") {
        return None;
    }
    first_ascii_numbers(value).into_iter().last()
}

fn episode_count_from_text(value: &str) -> Option<i64> {
    if !value.contains('集') {
        return None;
    }
    first_ascii_numbers(value).into_iter().last()
}

fn first_ascii_numbers(value: &str) -> Vec<i64> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(number) = current.parse::<i64>() {
                numbers.push(number);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(number) = current.parse::<i64>() {
            numbers.push(number);
        }
    }
    numbers
}

fn minutes_to_ticks(minutes: i64) -> i64 {
    minutes.saturating_mul(60).saturating_mul(10_000_000)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn fold_lookup_name(name: &str) -> String {
    name.trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '_' | '-'))
        .chars()
        .map(|ch| {
            if matches!(ch, '.' | '_' | '-') {
                ' '
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn image_extension(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "jpg",
        "png" => "png",
        "webp" => "webp",
        _ => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_douban_search_window_data() {
        let html = r#"<script>
window.__DATA__ = {"count":1,"items":[{"abstract":"中国大陆 / 动画 / 奇幻 / 冒险 / 牧神记 第1-52集 / 20分钟","abstract_2":"沈乐平 / 姚铭舜 / 李欣","cover_url":"https://img9.doubanio.com/view/photo/s_ratio_poster/public/p2916595576.jpg","id":36576581,"labels":[{"text":"剧集"}],"rating":{"value":8.8},"title":"牧神记 年番1‎ (2024)"}]};
</script>"#;
        let data = extract_window_data(html).unwrap();
        let subjects = parse_search_data(data).unwrap();
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].id, "36576581");
        assert_eq!(subjects[0].title, "牧神记 年番1");
        assert_eq!(subjects[0].item_type.as_deref(), Some("Series"));
        assert_eq!(subjects[0].year, Some(2024));
        assert_eq!(subjects[0].community_rating, Some(8.8));
        assert_eq!(subjects[0].runtime_ticks, Some(12_000_000_000));
        assert_eq!(subjects[0].episode_count, Some(52));
        assert_eq!(subjects[0].genres, vec!["动画", "奇幻", "冒险"]);
    }

    #[test]
    fn parses_mobile_subject_meta_and_intro() {
        let html = r#"
<meta itemprop="name" content="牧神记 年番1 - 电视剧">
<meta itemprop="description" content="牧神记 年番1豆瓣评分：8.8 简介：主角秦牧。">
<meta itemprop="image" content="https://qnmob3.doubanio.com/view/photo/large/public/p2916595576.jpg?imageView2/1/q/60">
<meta itemprop="ratingValue" content="8.8">
<div class="sub-meta">中国大陆 / 动画 / 奇幻 / 2024-10-27(中国大陆)上映 / 片长20分钟</div>
<section class="subject-intro"><h2>剧集简介</h2><div class="bd"><p>主角秦牧。</p></div></section>
<section class="subject-pics"><img data-src="https://img1.doubanio.com/view/photo/photo/public/p1.jpg"></section>
"#;
        let subject = parse_mobile_subject_html("36576581", html, "Series");
        assert_eq!(subject.title, "牧神记 年番1");
        assert_eq!(subject.item_type.as_deref(), Some("Series"));
        assert_eq!(subject.year, Some(2024));
        assert_eq!(subject.overview.as_deref(), Some("主角秦牧。"));
        assert_eq!(subject.community_rating, Some(8.8));
        assert_eq!(subject.genres, vec!["动画", "奇幻"]);
        assert_eq!(subject.countries, vec!["中国大陆"]);
        assert_eq!(subject.runtime_ticks, Some(12_000_000_000));
        assert_eq!(
            subject.image_url.as_deref(),
            Some("https://qnmob3.doubanio.com/view/photo/large/public/p2916595576.jpg")
        );
    }
}
