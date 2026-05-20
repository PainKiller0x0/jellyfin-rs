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
        .query(&[("api_key", api_key), ("language", "zh-CN"), ("append_to_response", "credits")])
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
        .map(|person| {
            let mut entry = json!({ "Name": person.name, "Role": person.character, "Type": "Actor" });
            if let Some(ref profile) = person.profile_path {
                entry["ImageUrl"] = json!(format!("https://image.tmdb.org/t/p/w185{profile}"));
            }
            entry
        })
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
        "ImageUrl": response.poster_path.map(|p| format!("https://image.tmdb.org/t/p/w500{p}")),
        "BackdropUrl": response.backdrop_path.map(|p| format!("https://image.tmdb.org/t/p/w1280{p}")),
    }))
}

pub async fn tmdb_tv_search(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
    year: Option<i64>,
) -> anyhow::Result<Vec<Value>> {
    let mut request = client
        .get("https://api.themoviedb.org/3/search/tv")
        .query(&[("api_key", api_key), ("query", name)]);
    let year_string;
    if let Some(year) = year {
        year_string = year.to_string();
        request = request.query(&[("first_air_date_year", year_string.as_str())]);
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
        .map(|show| {
            let year = show
                .first_air_date
                .as_deref()
                .and_then(|date| date.get(0..4))
                .and_then(|year| year.parse::<i64>().ok());
            json!({
                "Name": show.name,
                "ProductionYear": year,
                "SearchProviderName": "TheMovieDb",
                "ProviderIds": { "Tmdb": show.id.to_string() },
                "ImageUrl": show.poster_path.map(|path| format!("https://image.tmdb.org/t/p/w342{path}")),
                "Overview": show.overview,
            })
        })
        .collect())
}

pub async fn tmdb_tv_details(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
) -> anyhow::Result<Value> {
    let response = client
        .get(format!("https://api.themoviedb.org/3/tv/{tmdb_id}"))
        .query(&[
            ("api_key", api_key),
            ("language", "zh-CN"),
            ("append_to_response", "credits,external_ids"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbTvDetails>()
        .await?;

    let year = response
        .first_air_date
        .as_deref()
        .and_then(|date| date.get(0..4))
        .and_then(|year| year.parse::<i64>().ok());
    let cast = response
        .credits
        .map(|credits| credits.cast)
        .unwrap_or_default()
        .into_iter()
        .take(20)
        .map(|person| {
            let mut entry = json!({ "Name": person.name, "Role": person.roles.first().map(|r| r.character.clone()).unwrap_or_default(), "Type": "Actor" });
            if let Some(ref profile) = person.profile_path {
                entry["ImageUrl"] = json!(format!("https://image.tmdb.org/t/p/w185{profile}"));
            }
            entry
        })
        .collect::<Vec<_>>();

    let external_imdb = response
        .external_ids
        .as_ref()
        .and_then(|ids| ids.imdb_id.clone());
    let external_tvdb = response
        .external_ids
        .as_ref()
        .and_then(|ids| ids.tvdb_id.map(|id| id.to_string()));

    Ok(json!({
        "Name": response.name,
        "Overview": response.overview,
        "ProductionYear": year,
        "ProviderIds": {
            "Tmdb": response.id.to_string(),
            "IMDB": external_imdb.unwrap_or_default(),
            "Tvdb": external_tvdb.unwrap_or_default(),
        },
        "Genres": response.genres.into_iter().map(|genre| genre.name).collect::<Vec<_>>(),
        "Studios": response.networks.into_iter().map(|network| network.name).collect::<Vec<_>>(),
        "People": cast,
        "ImageUrl": response.poster_path.map(|p| format!("https://image.tmdb.org/t/p/w500{p}")),
        "BackdropUrl": response.backdrop_path.map(|p| format!("https://image.tmdb.org/t/p/w1280{p}")),
        "Status": response.status,
        "AirDays": response.episode_run_time.first().copied(),
        "SeasonCount": response.number_of_seasons,
        "EpisodeCount": response.number_of_episodes,
    }))
}

pub async fn tmdb_person_search(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
) -> anyhow::Result<Vec<Value>> {
    let response = client
        .get("https://api.themoviedb.org/3/search/person")
        .query(&[("api_key", api_key), ("query", name)])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbPersonSearchResponse>()
        .await?;

    Ok(response
        .results
        .into_iter()
        .take(20)
        .map(|person| {
            json!({
                "Name": person.name,
                "SearchProviderName": "TheMovieDb",
                "ProviderIds": { "Tmdb": person.id.to_string() },
                "ImageUrl": person.profile_path.map(|path| format!("https://image.tmdb.org/t/p/w185{path}")),
                "Overview": person.known_for_department,
            })
        })
        .collect())
}

pub async fn tmdb_person_details(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
) -> anyhow::Result<Value> {
    let response = client
        .get(format!("https://api.themoviedb.org/3/person/{tmdb_id}"))
        .query(&[
            ("api_key", api_key),
            ("append_to_response", "external_ids,combined_credits"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbPersonDetails>()
        .await?;

    let filmography: Vec<Value> = response
        .combined_credits
        .map(|credits| {
            credits
                .cast
                .into_iter()
                .map(|role| {
                    let year = role
                        .release_date
                        .or(role.first_air_date)
                        .and_then(|date| date.get(0..4).and_then(|y| y.parse::<i64>().ok()));
                    json!({
                        "Name": role.title.or(role.name),
                        "Type": role.media_type.map(|t| match t.as_str() {
                            "movie" => "Movie",
                            "tv" => "Series",
                            _ => "Unknown",
                        }),
                        "ProductionYear": year,
                        "Role": role.character,
                        "ProviderIds": {
                            "Tmdb": role.id.to_string(),
                        },
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "Name": response.name,
        "Overview": response.biography,
        "ProviderIds": {
            "Tmdb": response.id.to_string(),
            "IMDB": response.imdb_id.unwrap_or_default(),
        },
        "ImageUrl": response.profile_path.map(|path| format!("https://image.tmdb.org/t/p/w342{path}")),
        "BirthDate": response.birthday,
        "DeathDate": response.deathday,
        "PlaceOfBirth": response.place_of_birth,
        "KnownFor": response.known_for_department,
        "Filmography": filmography,
        "HomePageUrl": response.homepage,
    }))
}

#[allow(dead_code)]
pub async fn tmdb_person_images(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
) -> anyhow::Result<Vec<Value>> {
    #[derive(Deserialize)]
    struct TmdbPersonImages {
        #[serde(default)]
        profiles: Vec<TmdbImage>,
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
            "https://api.themoviedb.org/3/person/{tmdb_id}/images"
        ))
        .query(&[("api_key", api_key)])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbPersonImages>()
        .await?;

    Ok(response
        .profiles
        .into_iter()
        .map(|profile| {
            let thumbnail_url = format!("https://image.tmdb.org/t/p/w185{}", profile.file_path);
            let full_url = format!("https://image.tmdb.org/t/p/original{}", profile.file_path);
            let mut image = json!({
                "ProviderName": "TheMovieDb",
                "Url": full_url,
                "ThumbnailUrl": thumbnail_url,
                "Height": profile.height,
                "Width": profile.width,
                "CommunityRating": profile.vote_average,
                "VoteCount": profile.vote_count,
                "Type": "Primary",
            });
            if let Some(ref lang) = profile.iso_639_1 {
                if !lang.is_empty() {
                    image["Language"] = json!(lang);
                }
            }
            image
        })
        .collect())
}

#[derive(Deserialize)]
struct TmdbSearchResponse {
    results: Vec<TmdbSearchItem>,
}

#[derive(Deserialize)]
struct TmdbSearchItem {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    name: String,
    overview: Option<String>,
    poster_path: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
}

#[derive(Deserialize)]
struct TmdbMovieDetails {
    id: i64,
    title: String,
    overview: Option<String>,
    release_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    imdb_id: Option<String>,
    #[serde(default)]
    genres: Vec<TmdbNamedItem>,
    #[serde(default)]
    production_companies: Vec<TmdbNamedItem>,
    credits: Option<TmdbCredits>,
}

#[derive(Deserialize)]
struct TmdbTvDetails {
    id: i64,
    name: String,
    overview: Option<String>,
    first_air_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    status: Option<String>,
    number_of_seasons: Option<i64>,
    number_of_episodes: Option<i64>,
    #[serde(default)]
    episode_run_time: Vec<i64>,
    #[serde(default)]
    genres: Vec<TmdbNamedItem>,
    #[serde(default)]
    networks: Vec<TmdbNamedItem>,
    credits: Option<TmdbTvCredits>,
    external_ids: Option<TmdbExternalIds>,
}

#[derive(Deserialize)]
struct TmdbTvCredits {
    #[serde(default)]
    cast: Vec<TmdbTvCastMember>,
}

#[derive(Deserialize)]
struct TmdbTvCastMember {
    name: String,
    profile_path: Option<String>,
    #[serde(default)]
    roles: Vec<TmdbTvRole>,
}

#[derive(Deserialize)]
struct TmdbTvRole {
    character: String,
}

#[derive(Deserialize)]
struct TmdbExternalIds {
    imdb_id: Option<String>,
    tvdb_id: Option<i64>,
}

#[derive(Deserialize)]
struct TmdbPersonSearchResponse {
    results: Vec<TmdbPersonSearchItem>,
}

#[derive(Deserialize)]
struct TmdbPersonSearchItem {
    id: i64,
    name: String,
    profile_path: Option<String>,
    known_for_department: Option<String>,
}

#[derive(Deserialize)]
struct TmdbPersonDetails {
    id: i64,
    name: String,
    biography: Option<String>,
    birthday: Option<String>,
    deathday: Option<String>,
    place_of_birth: Option<String>,
    profile_path: Option<String>,
    imdb_id: Option<String>,
    known_for_department: Option<String>,
    homepage: Option<String>,
    combined_credits: Option<TmdbCombinedCredits>,
}

#[derive(Deserialize)]
struct TmdbCombinedCredits {
    #[serde(default)]
    cast: Vec<TmdbCombinedCredit>,
}

#[derive(Deserialize)]
struct TmdbCombinedCredit {
    id: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    character: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    media_type: Option<String>,
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
    profile_path: Option<String>,
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
