use std::path::Path;

#[derive(Default)]
pub struct ParsedMetadata {
    pub title: Option<String>,
    pub overview: Option<String>,
    pub production_year: Option<i64>,
    pub provider_ids: Vec<(String, String)>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub studios: Vec<String>,
    pub people: Vec<ParsedPerson>,
}

pub struct ParsedPerson {
    pub name: String,
    pub role: Option<String>,
    pub person_type: String,
}

pub async fn parse_sidecar_metadata(path: &Path) -> ParsedMetadata {
    let mut metadata = parse_filename_metadata(path);
    if let Some(nfo) = read_sidecar_nfo(path).await {
        merge_nfo_metadata(&mut metadata, &nfo);
    }
    metadata
}

fn parse_filename_metadata(path: &Path) -> ParsedMetadata {
    let title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .replace(['.', '_'], " ");
    let production_year = find_year(&title);
    let title = production_year
        .map(|year| {
            title
                .replace(&format!("({year})"), "")
                .replace(&format!("[{year}]"), "")
                .replace(&year.to_string(), "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or(title);

    ParsedMetadata {
        title: Some(title),
        production_year,
        ..Default::default()
    }
}

async fn read_sidecar_nfo(path: &Path) -> Option<String> {
    let stem_nfo = path.with_extension("nfo");
    if let Ok(contents) = tokio::fs::read_to_string(&stem_nfo).await {
        return Some(contents);
    }

    let movie_nfo = path.parent()?.join("movie.nfo");
    tokio::fs::read_to_string(movie_nfo).await.ok()
}

fn merge_nfo_metadata(metadata: &mut ParsedMetadata, nfo: &str) {
    metadata.title = first_tag(nfo, &["title", "originaltitle"]).or(metadata.title.take());
    metadata.overview = first_tag(nfo, &["plot", "outline", "overview"]);
    metadata.production_year = first_tag(nfo, &["year", "premiered", "releasedate"])
        .and_then(|value| find_year(&value))
        .or(metadata.production_year);

    metadata.genres = tags(nfo, "genre");
    metadata.tags = tags(nfo, "tag");
    metadata.studios = tags(nfo, "studio");
    metadata.people = actor_blocks(nfo);
    metadata.provider_ids = provider_ids(nfo);
}

fn provider_ids(nfo: &str) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    push_provider(&mut ids, "Tmdb", first_tag(nfo, &["tmdbid", "tmdb"]));
    push_provider(&mut ids, "Tvdb", first_tag(nfo, &["tvdbid", "tvdb"]));
    push_provider(&mut ids, "IMDB", first_tag(nfo, &["imdbid", "imdb"]));

    for block in blocks(nfo, "uniqueid") {
        let provider = attribute_value(&block.opening_tag, "type").unwrap_or_default();
        let key = match provider.to_ascii_lowercase().as_str() {
            "tmdb" => Some("Tmdb"),
            "tvdb" => Some("Tvdb"),
            "imdb" => Some("IMDB"),
            "musicbrainzalbum" | "musicbrainz album" => Some("MusicBrainzAlbum"),
            "musicbrainzalbumartist" | "musicbrainz album artist" => Some("MusicBrainzAlbumArtist"),
            "musicbrainzreleasegroup" | "musicbrainz release group" => {
                Some("MusicBrainzReleaseGroup")
            }
            _ => None,
        };
        push_provider(&mut ids, key.unwrap_or(&provider), Some(block.contents));
    }

    ids.sort();
    ids.dedup();
    ids
}

fn push_provider(ids: &mut Vec<(String, String)>, provider: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        ids.push((provider.to_string(), value.trim().to_string()));
    }
}

fn actor_blocks(nfo: &str) -> Vec<ParsedPerson> {
    blocks(nfo, "actor")
        .into_iter()
        .filter_map(|block| {
            let name = first_tag(&block.contents, &["name"])?;
            Some(ParsedPerson {
                name,
                role: first_tag(&block.contents, &["role"]),
                person_type: first_tag(&block.contents, &["type"])
                    .unwrap_or_else(|| "Actor".to_string()),
            })
        })
        .collect()
}

fn first_tag(contents: &str, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| tags(contents, name).into_iter().next())
}

fn tags(contents: &str, name: &str) -> Vec<String> {
    blocks(contents, name)
        .into_iter()
        .map(|block| decode_xml_text(&block.contents))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn blocks(contents: &str, name: &str) -> Vec<TagBlock> {
    let lower = contents.to_ascii_lowercase();
    let mut cursor = 0usize;
    let mut result = Vec::new();
    let open_prefix = format!("<{name}");
    let close = format!("</{name}>");

    while let Some(relative_open) = lower[cursor..].find(&open_prefix) {
        let open_start = cursor + relative_open;
        let Some(open_end_relative) = lower[open_start..].find('>') else {
            break;
        };
        let open_end = open_start + open_end_relative + 1;
        let Some(close_relative) = lower[open_end..].find(&close) else {
            break;
        };
        let close_start = open_end + close_relative;
        result.push(TagBlock {
            opening_tag: contents[open_start..open_end].to_string(),
            contents: contents[open_end..close_start].to_string(),
        });
        cursor = close_start + close.len();
    }

    result
}

fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let start = lower.find(&needle)? + needle.len();
    let quote = tag[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = start + quote.len_utf8();
    let value_end = tag[value_start..].find(quote)? + value_start;
    Some(tag[value_start..value_end].to_string())
}

fn find_year(value: &str) -> Option<i64> {
    value
        .as_bytes()
        .windows(4)
        .filter_map(|digits| std::str::from_utf8(digits).ok())
        .filter_map(|digits| digits.parse::<i64>().ok())
        .find(|year| (1880..=2100).contains(year))
}

fn decode_xml_text(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

struct TagBlock {
    opening_tag: String,
    contents: String,
}
