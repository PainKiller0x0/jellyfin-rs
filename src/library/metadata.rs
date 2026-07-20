use std::path::Path;

#[derive(Default)]
pub struct ParsedMetadata {
    pub title: Option<String>,
    pub overview: Option<String>,
    pub official_rating: Option<String>,
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
        provider_ids: provider_ids_from_path(path),
        ..Default::default()
    }
}

pub fn provider_ids_from_path(path: &Path) -> Vec<(String, String)> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        let mut rest = name;
        while let Some((_, after_open)) = rest.split_once(open) {
            let Some((tag, after_close)) = after_open.split_once(close) else {
                break;
            };
            if let Some((provider, id)) = provider_id_from_tag(tag) {
                upsert_provider_value(&mut ids, provider, id);
            }
            rest = after_close;
        }
    }
    ids.sort();
    ids
}

fn provider_id_from_tag(tag: &str) -> Option<(&'static str, String)> {
    let tag = tag.trim();
    let lower = tag.to_ascii_lowercase();
    for (provider, prefixes) in [
        ("Tmdb", ["tmdb-", "tmdbid-", "tmdbid="].as_slice()),
        ("Douban", ["douban-", "doubanid-", "doubanid="].as_slice()),
        ("IMDB", ["imdb-", "imdbid-", "imdbid="].as_slice()),
        ("Tvdb", ["tvdb-", "tvdbid-", "tvdbid="].as_slice()),
    ] {
        for prefix in prefixes {
            if lower.starts_with(prefix) {
                let value = tag[prefix.len()..].trim();
                if !value.is_empty() {
                    return Some((provider, value.to_string()));
                }
            }
        }
    }
    None
}

fn upsert_provider_value(ids: &mut Vec<(String, String)>, provider: &str, value: String) {
    ids.retain(|(existing, _)| existing != provider);
    ids.push((provider.to_string(), value));
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
    metadata.official_rating = first_tag(
        nfo,
        &[
            "mpaa",
            "contentrating",
            "content_rating",
            "rating",
            "certification",
        ],
    );
    metadata.production_year = first_tag(nfo, &["year", "premiered", "releasedate"])
        .and_then(|value| find_year(&value))
        .or(metadata.production_year);

    metadata.genres = tags(nfo, "genre");
    metadata.tags = tags(nfo, "tag");
    metadata.studios = tags(nfo, "studio");
    metadata.people = actor_blocks(nfo);
    for (provider, provider_item_id) in provider_ids(nfo) {
        upsert_provider_value(&mut metadata.provider_ids, &provider, provider_item_id);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_provider_ids_from_path_tags() {
        let ids = provider_ids_from_path(Path::new(
            "/media/Movie (2024) [tmdbid-123] {douban-456}.strm",
        ));
        assert_eq!(
            ids,
            vec![
                ("Douban".to_string(), "456".to_string()),
                ("Tmdb".to_string(), "123".to_string())
            ]
        );
    }

    #[test]
    fn nfo_provider_ids_override_path_provider_ids() {
        let mut metadata = ParsedMetadata {
            provider_ids: provider_ids_from_path(Path::new("Movie {tmdb-1}.strm")),
            ..Default::default()
        };
        merge_nfo_metadata(
            &mut metadata,
            r#"<movie><uniqueid type="tmdb">2</uniqueid></movie>"#,
        );
        assert_eq!(
            metadata.provider_ids,
            vec![("Tmdb".to_string(), "2".to_string())]
        );
    }
}

struct TagBlock {
    opening_tag: String,
    contents: String,
}
