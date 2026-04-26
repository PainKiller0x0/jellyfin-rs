use serde::Deserialize;
use serde_json::{Value, json};

pub async fn tmdb_movie_search(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
    year: Option<i64>,
) -> anyhow::Result<Vec<Value>> {
    let mut request = client
        .get("https://api.themoviedb.org/3/search/movie")
        .query(&[("api_key", api_key), ("query", name)]);
    let year_string;
    if let Some(year) = year {
        year_string = year.to_string();
        request = request.query(&[("year", year_string.as_str())]);
    }

    let response = request
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbSearchResponse>()
        .await?;

    Ok(response
        .results
        .into_iter()
        .take(20)
        .map(|movie| {
            let year = movie
                .release_date
                .as_deref()
                .and_then(|date| date.get(0..4))
                .and_then(|year| year.parse::<i64>().ok());
            json!({
                "Name": movie.title,
                "ProductionYear": year,
                "SearchProviderName": "TheMovieDb",
                "ProviderIds": { "Tmdb": movie.id.to_string() },
                "ImageUrl": movie.poster_path.map(|path| format!("https://image.tmdb.org/t/p/w342{path}")),
                "Overview": movie.overview,
            })
        })
        .collect())
}

pub async fn tmdb_movie_details(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
) -> anyhow::Result<Value> {
    let response = client
        .get(format!("https://api.themoviedb.org/3/movie/{tmdb_id}"))
        .query(&[("api_key", api_key), ("append_to_response", "credits")])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbMovieDetails>()
        .await?;

    let year = response
        .release_date
        .as_deref()
        .and_then(|date| date.get(0..4))
        .and_then(|year| year.parse::<i64>().ok());
    let cast = response
        .credits
        .map(|credits| credits.cast)
        .unwrap_or_default()
        .into_iter()
        .take(20)
        .map(|person| json!({ "Name": person.name, "Role": person.character, "Type": "Actor" }))
        .collect::<Vec<_>>();

    Ok(json!({
        "Name": response.title,
        "Overview": response.overview,
        "ProductionYear": year,
        "ProviderIds": {
            "Tmdb": response.id.to_string(),
            "IMDB": response.imdb_id.unwrap_or_default()
        },
        "Genres": response.genres.into_iter().map(|genre| genre.name).collect::<Vec<_>>(),
        "Studios": response.production_companies.into_iter().map(|company| company.name).collect::<Vec<_>>(),
        "People": cast,
    }))
}

#[derive(Deserialize)]
struct TmdbSearchResponse {
    results: Vec<TmdbMovie>,
}

#[derive(Deserialize)]
struct TmdbMovie {
    id: i64,
    title: String,
    overview: Option<String>,
    poster_path: Option<String>,
    release_date: Option<String>,
}

#[derive(Deserialize)]
struct TmdbMovieDetails {
    id: i64,
    title: String,
    overview: Option<String>,
    release_date: Option<String>,
    imdb_id: Option<String>,
    #[serde(default)]
    genres: Vec<TmdbNamedItem>,
    #[serde(default)]
    production_companies: Vec<TmdbNamedItem>,
    credits: Option<TmdbCredits>,
}

#[derive(Deserialize)]
struct TmdbNamedItem {
    name: String,
}

#[derive(Deserialize)]
struct TmdbCredits {
    #[serde(default)]
    cast: Vec<TmdbCastMember>,
}

#[derive(Deserialize)]
struct TmdbCastMember {
    name: String,
    character: Option<String>,
}

pub async fn tmdb_movie_images(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
) -> anyhow::Result<Vec<Value>> {
    #[derive(Deserialize)]
    struct TmdbImages {
        #[serde(default)]
        backdrops: Vec<TmdbImage>,
        #[serde(default)]
        posters: Vec<TmdbImage>,
    }
    #[derive(Deserialize)]
    struct TmdbImage {
        file_path: String,
        width: Option<i64>,
        height: Option<i64>,
        vote_average: Option<f64>,
        vote_count: Option<i64>,
        iso_639_1: Option<String>,
    }

    let response = client
        .get(format!(
            "https://api.themoviedb.org/3/movie/{tmdb_id}/images"
        ))
        .query(&[("api_key", api_key)])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbImages>()
        .await?;

    let mut images: Vec<Value> = Vec::new();

    for poster in &response.posters {
        let thumbnail_url = format!("https://image.tmdb.org/t/p/w342{}", poster.file_path);
        let full_url = format!("https://image.tmdb.org/t/p/original{}", poster.file_path);
        let mut image = json!({
            "ProviderName": "TheMovieDb",
            "Url": full_url,
            "ThumbnailUrl": thumbnail_url,
            "Height": poster.height,
            "Width": poster.width,
            "CommunityRating": poster.vote_average,
            "VoteCount": poster.vote_count,
            "Type": "Primary",
        });
        if let Some(ref lang) = poster.iso_639_1 {
            if !lang.is_empty() {
                image["Language"] = json!(lang);
            }
        }
        images.push(image);
    }

    for backdrop in &response.backdrops {
        let thumbnail_url = format!("https://image.tmdb.org/t/p/w342{}", backdrop.file_path);
        let full_url = format!("https://image.tmdb.org/t/p/original{}", backdrop.file_path);
        let mut image = json!({
            "ProviderName": "TheMovieDb",
            "Url": full_url,
            "ThumbnailUrl": thumbnail_url,
            "Height": backdrop.height,
            "Width": backdrop.width,
            "CommunityRating": backdrop.vote_average,
            "VoteCount": backdrop.vote_count,
            "Type": "Backdrop",
        });
        if let Some(ref lang) = backdrop.iso_639_1 {
            if !lang.is_empty() {
                image["Language"] = json!(lang);
            }
        }
        images.push(image);
    }

    Ok(images)
}
