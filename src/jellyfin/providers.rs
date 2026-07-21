use serde::Deserialize;
use serde_json::{Value, json};

use crate::{tmdb, util::normalize_yyyy_mm_dd};

pub async fn tmdb_movie_search(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
    year: Option<i64>,
    base_url: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let mut request = client
        .get(tmdb::api_url(base_url, "search/movie"))
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
                "Type": "Movie",
                "ProductionYear": year,
                "PremiereDate": movie.release_date.as_deref().and_then(normalize_yyyy_mm_dd),
                "SearchProviderName": "TheMovieDb",
                "ProviderIds": { "Tmdb": movie.id.to_string() },
                "ImageUrl": movie.poster_path.map(|path| tmdb::image_url(base_url, "w342", &path)),
                "Overview": movie.overview,
            })
        })
        .collect())
}

pub async fn tmdb_movie_details(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
    base_url: Option<&str>,
    language: &str,
    country_code: &str,
) -> anyhow::Result<Value> {
    let response = client
        .get(tmdb::api_url(base_url, &format!("movie/{tmdb_id}")))
        .query(&[
            ("api_key", api_key),
            ("language", language),
            (
                "append_to_response",
                "credits,release_dates,keywords,videos",
            ),
        ])
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
    let credits = response.credits.unwrap_or_default();
    let mut people = credits
        .cast
        .into_iter()
        .take(15)
        .map(|person| {
            let mut entry = json!({
                "Name": person.name,
                "Role": person.character,
                "Type": "Actor",
                "SortOrder": person.order,
                "ProviderIds": { "Tmdb": person.id.to_string() }
            });
            if let Some(ref profile) = person.profile_path {
                entry["ImageUrl"] = json!(tmdb::image_url(base_url, "w185", profile));
            }
            entry
        })
        .collect::<Vec<_>>();
    people.extend(
        credits
            .crew
            .into_iter()
            .filter_map(|person| {
                let person_type = tmdb_crew_person_type(
                    person.department.as_deref().unwrap_or_default(),
                    person.job.as_deref().unwrap_or_default(),
                )?;
                let mut entry = json!({
                    "Name": person.name,
                    "Role": person.job,
                    "Type": person_type,
                    "ProviderIds": { "Tmdb": person.id.to_string() }
                });
                if let Some(ref profile) = person.profile_path {
                    entry["ImageUrl"] = json!(tmdb::image_url(base_url, "w185", profile));
                }
                Some(entry)
            })
            .take(15),
    );

    // Jellyfin prefers the item's metadata country, then US.
    let official_rating = response.release_dates.as_ref().and_then(|rd| {
        let releases = rd
            .results
            .iter()
            .filter_map(|result| {
                result
                    .release_dates
                    .iter()
                    .find_map(|release| {
                        release
                            .certification
                            .as_deref()
                            .map(str::trim)
                            .filter(|rating| !rating.is_empty())
                    })
                    .map(|rating| (result.iso_3166_1.as_deref().unwrap_or_default(), rating))
            })
            .collect::<Vec<_>>();
        releases
            .iter()
            .find(|(country, _)| country.eq_ignore_ascii_case(country_code))
            .map(|(_, rating)| build_parental_rating(country_code, rating))
            .or_else(|| {
                releases
                    .iter()
                    .find(|(country, _)| country.eq_ignore_ascii_case("US"))
                    .map(|(_, rating)| (*rating).to_string())
            })
    });

    let countries: Vec<String> = response
        .production_countries
        .iter()
        .filter_map(|c| c.name.clone())
        .collect();

    let languages: Vec<String> = response
        .spoken_languages
        .iter()
        .filter_map(|l| l.name.clone())
        .collect();
    let mut trailer_videos = response
        .videos
        .as_ref()
        .map(|videos| videos.results.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    trailer_videos.sort_by_key(|video| !video.video_type.eq_ignore_ascii_case("trailer"));
    let remote_trailers = trailer_videos
        .into_iter()
        .filter(|video| tmdb_video_is_trailer(video))
        .map(|video| {
            json!({
                "Name": video.name,
                "Url": format!("https://www.youtube.com/watch?v={}", video.key)
            })
        })
        .collect::<Vec<_>>();
    let tmdb_collection_id = response
        .belongs_to_collection
        .as_ref()
        .map(|collection| collection.id.to_string())
        .unwrap_or_default();
    let collection_name = response
        .belongs_to_collection
        .as_ref()
        .map(|collection| collection.name.clone());

    Ok(json!({
        "Name": response.title,
        "OriginalTitle": response.original_title,
        "Type": "Movie",
        "Overview": response.overview,
        "ProductionYear": year,
        "PremiereDate": response.release_date.as_deref().and_then(normalize_yyyy_mm_dd),
        "CommunityRating": response.vote_average,
        "OfficialRating": official_rating,
        "Tagline": response.tagline,
        "ProductionLocations": countries,
        "RuntimeTicks": response.runtime.map(|m| m * 60 * 10_000_000),
        "Status": response.status,
        "OriginalLanguage": response.original_language,
        "CollectionName": collection_name,
        "RemoteTrailers": remote_trailers,
        "Countries": countries,
        "Languages": languages,
        "ProviderIds": {
            "Tmdb": response.id.to_string(),
            "IMDB": response.imdb_id.unwrap_or_default(),
            "TmdbCollection": tmdb_collection_id
        },
        "Genres": response.genres.into_iter().map(|genre| genre.name).collect::<Vec<_>>(),
        "Tags": response.keywords.map(|keywords| keywords.keywords.into_iter().map(|keyword| keyword.name).collect::<Vec<_>>()).unwrap_or_default(),
        "Studios": response.production_companies.into_iter().map(|company| company.name).collect::<Vec<_>>(),
        "People": people,
        "ImageUrl": response.poster_path.map(|p| tmdb::image_url(base_url, "w500", &p)),
        "BackdropUrl": response.backdrop_path.map(|p| tmdb::image_url(base_url, "w1280", &p)),
    }))
}

pub async fn tmdb_tv_search(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
    year: Option<i64>,
    base_url: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let mut request = client
        .get(tmdb::api_url(base_url, "search/tv"))
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
                "Type": "Series",
                "ProductionYear": year,
                "PremiereDate": show.first_air_date.as_deref().and_then(normalize_yyyy_mm_dd),
                "SearchProviderName": "TheMovieDb",
                "ProviderIds": { "Tmdb": show.id.to_string() },
                "ImageUrl": show.poster_path.map(|path| tmdb::image_url(base_url, "w342", &path)),
                "Overview": show.overview,
            })
        })
        .collect())
}

pub async fn tmdb_tv_details(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
    base_url: Option<&str>,
    language: &str,
    country_code: &str,
) -> anyhow::Result<Value> {
    let response = client
        .get(tmdb::api_url(base_url, &format!("tv/{tmdb_id}")))
        .query(&[
            ("api_key", api_key),
            ("language", language),
            (
                "append_to_response",
                "credits,external_ids,content_ratings,keywords,videos",
            ),
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
    let credits = response.credits.unwrap_or_default();
    let mut people = credits
        .cast
        .into_iter()
        .take(15)
        .map(|person| {
            let mut entry = json!({
                "Name": person.name,
                "Role": person.character.or_else(|| person.roles.first().map(|r| r.character.clone())).unwrap_or_default(),
                "Type": "Actor",
                "SortOrder": person.order,
                "ProviderIds": { "Tmdb": person.id.to_string() }
            });
            if let Some(ref profile) = person.profile_path {
                entry["ImageUrl"] = json!(tmdb::image_url(base_url, "w185", profile));
            }
            entry
        })
        .collect::<Vec<_>>();
    people.extend(
        credits
            .crew
            .into_iter()
            .filter_map(|person| {
                let person_type = tmdb_crew_person_type(
                    person.department.as_deref().unwrap_or_default(),
                    person.job.as_deref().unwrap_or_default(),
                )?;
                let mut entry = json!({
                    "Name": person.name,
                    "Role": person.job,
                    "Type": person_type,
                    "ProviderIds": { "Tmdb": person.id.to_string() }
                });
                if let Some(ref profile) = person.profile_path {
                    entry["ImageUrl"] = json!(tmdb::image_url(base_url, "w185", profile));
                }
                Some(entry)
            })
            .take(15),
    );
    people.extend(response.created_by.iter().map(|person| {
        let mut entry = json!({
            "Name": person.name,
            "Role": "",
            "Type": "Creator",
            "ProviderIds": { "Tmdb": person.id.to_string() }
        });
        if let Some(ref profile) = person.profile_path {
            entry["ImageUrl"] = json!(tmdb::image_url(base_url, "w185", profile));
        }
        entry
    }));

    let external_imdb = response
        .external_ids
        .as_ref()
        .and_then(|ids| ids.imdb_id.clone());
    let external_tvdb = response
        .external_ids
        .as_ref()
        .and_then(|ids| ids.tvdb_id.map(|id| id.to_string()));

    // Jellyfin prefers the item's metadata country, then US, then first.
    let official_rating = response.content_ratings.as_ref().and_then(|cr| {
        let ratings = cr
            .results
            .iter()
            .filter_map(|result| {
                result
                    .rating
                    .as_deref()
                    .map(str::trim)
                    .filter(|rating| !rating.is_empty())
                    .map(|rating| (result.iso_3166_1.as_deref().unwrap_or_default(), rating))
            })
            .collect::<Vec<_>>();
        ratings
            .iter()
            .find(|(country, _)| country.eq_ignore_ascii_case(country_code))
            .map(|(_, rating)| build_parental_rating(country_code, rating))
            .or_else(|| {
                ratings
                    .iter()
                    .find(|(country, _)| country.eq_ignore_ascii_case("US"))
                    .map(|(_, rating)| (*rating).to_string())
            })
            .or_else(|| ratings.first().map(|(_, rating)| (*rating).to_string()))
    });

    let runtime_minutes = response.episode_run_time.first().copied();
    let countries: Vec<String> = response.origin_country.clone();
    let languages: Vec<String> = response
        .spoken_languages
        .iter()
        .filter_map(|l| l.name.clone())
        .collect();
    let studios = response
        .networks
        .iter()
        .chain(response.production_companies.iter())
        .map(|company| company.name.clone())
        .collect::<Vec<_>>();
    let remote_trailers = response
        .videos
        .as_ref()
        .into_iter()
        .flat_map(|videos| videos.results.iter())
        .filter(|video| tmdb_video_is_trailer(video))
        .map(|video| {
            json!({
                "Name": video.name,
                "Url": format!("https://www.youtube.com/watch?v={}", video.key)
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "Name": response.name,
        "OriginalTitle": response.original_name,
        "Type": "Series",
        "Overview": response.overview,
        "ProductionYear": year,
        "PremiereDate": response.first_air_date.as_deref().and_then(normalize_yyyy_mm_dd),
        "EndDate": response.last_air_date.as_deref().and_then(normalize_yyyy_mm_dd),
        "CommunityRating": response.vote_average,
        "OfficialRating": official_rating,
        "Tagline": response.tagline,
        "RuntimeTicks": runtime_minutes.map(|m| m * 60 * 10_000_000),
        "OriginalLanguage": response.original_language,
        "HomePageUrl": response.homepage,
        "RemoteTrailers": remote_trailers,
        "Countries": countries,
        "Languages": languages,
        "ProviderIds": {
            "Tmdb": response.id.to_string(),
            "IMDB": external_imdb.unwrap_or_default(),
            "Tvdb": external_tvdb.unwrap_or_default(),
        },
        "Genres": response.genres.into_iter().map(|genre| genre.name).collect::<Vec<_>>(),
        "Tags": response.keywords.map(|keywords| keywords.results.into_iter().map(|keyword| keyword.name).collect::<Vec<_>>()).unwrap_or_default(),
        "Studios": studios,
        "People": people,
        "ImageUrl": response.poster_path.map(|p| tmdb::image_url(base_url, "w500", &p)),
        "BackdropUrl": response.backdrop_path.map(|p| tmdb::image_url(base_url, "w1280", &p)),
        "Status": response.status.as_deref().and_then(normalize_tmdb_series_status),
        "SeasonCount": response.number_of_seasons,
        "EpisodeCount": response.number_of_episodes,
    }))
}

pub async fn tmdb_person_search(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
    base_url: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let response = client
        .get(tmdb::api_url(base_url, "search/person"))
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
                "Type": "Person",
                "SearchProviderName": "TheMovieDb",
                "ProviderIds": { "Tmdb": person.id.to_string() },
                "ImageUrl": person.profile_path.map(|path| tmdb::image_url(base_url, "w185", &path)),
                "Overview": person.known_for_department,
            })
        })
        .collect())
}

pub async fn tmdb_person_details(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
    base_url: Option<&str>,
) -> anyhow::Result<Value> {
    let response = client
        .get(tmdb::api_url(base_url, &format!("person/{tmdb_id}")))
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
        "Type": "Person",
        "Overview": response.biography,
        "ProviderIds": {
            "Tmdb": response.id.to_string(),
            "IMDB": response.imdb_id.unwrap_or_default(),
        },
        "ImageUrl": response.profile_path.map(|path| tmdb::image_url(base_url, "w342", &path)),
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
    base_url: Option<&str>,
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
        .get(tmdb::api_url(base_url, &format!("person/{tmdb_id}/images")))
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
            let thumbnail_url = tmdb::image_url(base_url, "w185", &profile.file_path);
            let full_url = tmdb::image_url(base_url, "original", &profile.file_path);
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
    original_title: Option<String>,
    overview: Option<String>,
    release_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    imdb_id: Option<String>,
    vote_average: Option<f64>,
    tagline: Option<String>,
    runtime: Option<i64>,
    status: Option<String>,
    original_language: Option<String>,
    belongs_to_collection: Option<TmdbCollection>,
    #[allow(dead_code)]
    budget: Option<i64>,
    #[allow(dead_code)]
    revenue: Option<i64>,
    #[serde(default)]
    genres: Vec<TmdbNamedItem>,
    #[serde(default)]
    production_companies: Vec<TmdbNamedItem>,
    #[serde(default)]
    production_countries: Vec<TmdbCountry>,
    #[serde(default)]
    spoken_languages: Vec<TmdbLanguage>,
    credits: Option<TmdbCredits>,
    release_dates: Option<TmdbReleaseDates>,
    keywords: Option<TmdbMovieKeywords>,
    videos: Option<TmdbVideos>,
}

#[derive(Deserialize)]
struct TmdbTvDetails {
    id: i64,
    name: String,
    original_name: Option<String>,
    overview: Option<String>,
    first_air_date: Option<String>,
    last_air_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    status: Option<String>,
    number_of_seasons: Option<i64>,
    number_of_episodes: Option<i64>,
    vote_average: Option<f64>,
    tagline: Option<String>,
    original_language: Option<String>,
    homepage: Option<String>,
    #[serde(default)]
    episode_run_time: Vec<i64>,
    #[serde(default)]
    genres: Vec<TmdbNamedItem>,
    #[serde(default)]
    networks: Vec<TmdbNamedItem>,
    #[serde(default)]
    production_companies: Vec<TmdbNamedItem>,
    #[serde(default)]
    origin_country: Vec<String>,
    #[serde(default)]
    spoken_languages: Vec<TmdbLanguage>,
    credits: Option<TmdbTvCredits>,
    external_ids: Option<TmdbExternalIds>,
    content_ratings: Option<TmdbContentRatings>,
    keywords: Option<TmdbTvKeywords>,
    #[serde(default)]
    created_by: Vec<TmdbCreatedBy>,
    videos: Option<TmdbVideos>,
}

#[derive(Deserialize)]
struct TmdbCollection {
    id: i64,
    name: String,
}

#[derive(Deserialize)]
struct TmdbVideos {
    #[serde(default)]
    results: Vec<TmdbVideo>,
}

#[derive(Deserialize)]
struct TmdbVideo {
    key: String,
    name: String,
    site: String,
    #[serde(rename = "type")]
    video_type: String,
}

fn tmdb_video_is_trailer(video: &TmdbVideo) -> bool {
    video.site.eq_ignore_ascii_case("youtube")
        && (video.video_type.eq_ignore_ascii_case("trailer")
            || video.video_type.eq_ignore_ascii_case("teaser"))
}

fn build_parental_rating(country_code: &str, rating: &str) -> String {
    let rating = rating.trim();
    if country_code.eq_ignore_ascii_case("US") {
        return rating.to_string();
    }
    if country_code.eq_ignore_ascii_case("DE") {
        return format!("FSK-{rating}");
    }
    format!("{}-{rating}", country_code.to_ascii_uppercase())
}

fn normalize_tmdb_series_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "continuing" | "pilot" | "returning" | "returning series" => Some("Continuing"),
        "ended" | "cancelled" | "canceled" => Some("Ended"),
        "unreleased" => Some("Unreleased"),
        _ => None,
    }
}

#[derive(Default, Deserialize)]
struct TmdbTvCredits {
    #[serde(default)]
    cast: Vec<TmdbTvCastMember>,
    #[serde(default)]
    crew: Vec<TmdbCrewMember>,
}

#[derive(Deserialize)]
struct TmdbTvCastMember {
    id: i64,
    name: String,
    character: Option<String>,
    profile_path: Option<String>,
    #[serde(default)]
    order: i64,
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
struct TmdbCountry {
    #[allow(dead_code)]
    iso_3166_1: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct TmdbLanguage {
    #[allow(dead_code)]
    iso_639_1: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct TmdbMovieKeywords {
    #[serde(default)]
    keywords: Vec<TmdbKeyword>,
}

#[derive(Deserialize)]
struct TmdbTvKeywords {
    #[serde(default)]
    results: Vec<TmdbKeyword>,
}

#[derive(Deserialize)]
struct TmdbKeyword {
    name: String,
}

#[derive(Deserialize)]
struct TmdbCreatedBy {
    id: i64,
    name: String,
    profile_path: Option<String>,
}

#[derive(Deserialize)]
struct TmdbReleaseDates {
    #[serde(default)]
    results: Vec<TmdbReleaseDateResult>,
}

#[derive(Deserialize)]
struct TmdbReleaseDateResult {
    iso_3166_1: Option<String>,
    #[serde(default)]
    release_dates: Vec<TmdbReleaseDateEntry>,
}

#[derive(Deserialize)]
struct TmdbReleaseDateEntry {
    certification: Option<String>,
}

#[derive(Deserialize)]
struct TmdbContentRatings {
    #[serde(default)]
    results: Vec<TmdbContentRatingResult>,
}

#[derive(Deserialize)]
struct TmdbContentRatingResult {
    iso_3166_1: Option<String>,
    rating: Option<String>,
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

#[derive(Default, Deserialize)]
struct TmdbCredits {
    #[serde(default)]
    cast: Vec<TmdbCastMember>,
    #[serde(default)]
    crew: Vec<TmdbCrewMember>,
}

#[derive(Deserialize)]
struct TmdbCastMember {
    id: i64,
    name: String,
    character: Option<String>,
    profile_path: Option<String>,
    #[serde(default)]
    order: i64,
}

#[derive(Deserialize)]
struct TmdbCrewMember {
    id: i64,
    name: String,
    department: Option<String>,
    job: Option<String>,
    profile_path: Option<String>,
}

pub(crate) fn tmdb_crew_person_type(department: &str, job: &str) -> Option<&'static str> {
    if department.eq_ignore_ascii_case("directing") && job.eq_ignore_ascii_case("director") {
        Some("Director")
    } else if department.eq_ignore_ascii_case("production") && job.eq_ignore_ascii_case("producer")
    {
        Some("Producer")
    } else if department.eq_ignore_ascii_case("writing")
        && ["writer", "screenplay", "novel"]
            .iter()
            .any(|candidate| job.eq_ignore_ascii_case(candidate))
    {
        Some("Writer")
    } else {
        None
    }
}

pub async fn tmdb_movie_images(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
    base_url: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    #[derive(Deserialize)]
    struct TmdbImages {
        #[serde(default)]
        backdrops: Vec<TmdbImage>,
        #[serde(default)]
        posters: Vec<TmdbImage>,
        #[serde(default)]
        logos: Vec<TmdbImage>,
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
        .get(tmdb::api_url(base_url, &format!("movie/{tmdb_id}/images")))
        .query(&[("api_key", api_key)])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbImages>()
        .await?;

    let mut images: Vec<Value> = Vec::new();

    for poster in &response.posters {
        let thumbnail_url = tmdb::image_url(base_url, "w342", &poster.file_path);
        let full_url = tmdb::image_url(base_url, "original", &poster.file_path);
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
        let thumbnail_url = tmdb::image_url(base_url, "w342", &backdrop.file_path);
        let full_url = tmdb::image_url(base_url, "original", &backdrop.file_path);
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

    for logo in &response.logos {
        let thumbnail_url = tmdb::image_url(base_url, "w500", &logo.file_path);
        let full_url = tmdb::image_url(base_url, "original", &logo.file_path);
        let mut image = json!({
            "ProviderName": "TheMovieDb",
            "Url": full_url,
            "ThumbnailUrl": thumbnail_url,
            "Height": logo.height,
            "Width": logo.width,
            "CommunityRating": logo.vote_average,
            "VoteCount": logo.vote_count,
            "Type": "Logo",
        });
        if let Some(ref lang) = logo.iso_639_1 {
            if !lang.is_empty() {
                image["Language"] = json!(lang);
            }
        }
        images.push(image);
    }

    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::{
        TmdbVideo, build_parental_rating, normalize_tmdb_series_status, tmdb_crew_person_type,
        tmdb_video_is_trailer,
    };

    #[test]
    fn crew_mapping_matches_jellyfin_tmdb_provider() {
        assert_eq!(
            tmdb_crew_person_type("Directing", "Director"),
            Some("Director")
        );
        assert_eq!(
            tmdb_crew_person_type("Production", "Producer"),
            Some("Producer")
        );
        for job in ["Writer", "Screenplay", "Novel"] {
            assert_eq!(tmdb_crew_person_type("Writing", job), Some("Writer"));
        }
        assert_eq!(tmdb_crew_person_type("Camera", "Director"), None);
    }

    #[test]
    fn trailer_and_series_status_mapping_match_jellyfin_tmdb_provider() {
        let trailer = TmdbVideo {
            key: "abc".to_string(),
            name: "Official Trailer".to_string(),
            site: "YouTube".to_string(),
            video_type: "Teaser".to_string(),
        };
        assert!(tmdb_video_is_trailer(&trailer));
        assert_eq!(
            normalize_tmdb_series_status("Returning Series"),
            Some("Continuing")
        );
        assert_eq!(normalize_tmdb_series_status("Canceled"), Some("Ended"));
        assert_eq!(normalize_tmdb_series_status("In Production"), None);
    }

    #[test]
    fn parental_rating_prefix_matches_jellyfin_tmdb_provider() {
        assert_eq!(build_parental_rating("US", "TV-14"), "TV-14");
        assert_eq!(build_parental_rating("CN", "IIA"), "CN-IIA");
        assert_eq!(build_parental_rating("DE", "12"), "FSK-12");
    }
}
