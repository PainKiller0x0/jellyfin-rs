use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    jellyfin::{
        auth::query_user_id_or_request,
        common::{internal_error, strip_nulls},
        item_queries::{
            latest_media_items, library_views, list_media_items, list_trailers, resume_media_items,
        },
    },
    library::models::MediaItem,
    util::stable_text_id,
};

pub async fn views(State(state): State<Arc<AppState>>) -> Response {
    match library_views(&state.db).await {
        Ok(items) => {
            let items: Vec<_> = items.into_iter().map(strip_nulls).collect();
            Json(base_item_query_result(items, 0)).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn items(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    list_items_response(state, user_id, query).await
}

pub async fn user_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_items_response(state, user_id, query).await
}

async fn list_items_response(
    state: Arc<AppState>,
    user_id: String,
    query: HashMap<String, String>,
) -> Response {
    match list_media_items(&state.db, &user_id, &query).await {
        Ok((items, total)) => {
            let json_items = super::enrich_episode_list(&state.db, &user_id, items).await;
            Json(base_item_query_result_with_total(
                json_items,
                total,
                query_start_index(&query),
            ))
            .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let parent_id = query_value(&query, &["ParentId", "parentId"]);
    match latest_media_items(&state.db, &user_id, parent_id).await {
        Ok(items) => {
            let json_items = super::enrich_episode_list(&state.db, &user_id, items).await;
            Json(json_items).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items_root(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let parent_id = query_value(&query, &["ParentId", "parentId"]);
    match latest_media_items(&state.db, &user_id, parent_id).await {
        Ok(items) => {
            let json_items = super::enrich_episode_list(&state.db, &user_id, items).await;
            Json(json_items).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn resume_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match resume_media_items(&state.db, &user_id).await {
        Ok(items) => {
            let total = items.len();
            let start = query_start_index(&query);
            let limit = query_limit(&query, total);
            let page = items.into_iter().skip(start).take(limit).collect();
            let enriched = super::enrich_resume_items(&state.db, page).await;
            Json(base_item_query_result_with_total(enriched, total, start)).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn items_root(
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    Json(root_folder_emby_value(&user_id)).into_response()
}

pub async fn user_items_root(Path(user_id): Path<String>) -> Response {
    Json(root_folder_emby_value(&user_id)).into_response()
}

pub async fn trailers(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    match list_trailers(&state.db, &user_id, &query).await {
        Ok((items, total)) => {
            media_list_response_with_total(items, total, query_start_index(&query))
        }
        Err(error) => internal_error(error),
    }
}

pub fn media_list_response(items: Vec<MediaItem>) -> Response {
    let total = items.len();
    Json(media_query_result(items, total, 0)).into_response()
}

pub fn media_list_response_with_total(
    items: Vec<MediaItem>,
    total: usize,
    start_index: usize,
) -> Response {
    Json(media_query_result(items, total, start_index)).into_response()
}

fn media_query_result(items: Vec<MediaItem>, total: usize, start_index: usize) -> Value {
    base_item_query_result_with_total(
        items
            .into_iter()
            .map(|item| strip_nulls(item.to_jellyfin_json()))
            .collect(),
        total,
        start_index,
    )
}

fn base_item_query_result(items: Vec<Value>, start_index: usize) -> Value {
    let total = items.len();
    base_item_query_result_with_total(items, total, start_index)
}

fn base_item_query_result_with_total(items: Vec<Value>, total: usize, start_index: usize) -> Value {
    json!({
        "Items": items,
        "TotalRecordCount": total,
        "StartIndex": start_index
    })
}

fn query_start_index(query: &HashMap<String, String>) -> usize {
    query_usize(query, "StartIndex", 0)
}

fn query_limit(query: &HashMap<String, String>, default: usize) -> usize {
    query_usize(query, "Limit", default).min(200)
}

fn query_usize(query: &HashMap<String, String>, key: &str, default: usize) -> usize {
    query
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn query_value<'a>(query: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    query
        .iter()
        .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

fn root_folder_value(user_id: &str) -> Value {
    let id = stable_text_id(&format!("user-root:{user_id}"));
    json!({
        "Name": "Root",
        "Id": id,
        "Type": "UserRootFolder",
        "UserId": user_id,
        "IsFolder": true,
        "ParentId": "",
        "Path": "",
        "ServerId": "jellyfin-rs",
        "CollectionType": "",
        "LocationType": "Virtual",
        "PlayAccess": "Full",
        "CanDelete": false,
        "CanDownload": false,
        "ProviderIds": {},
        "ImageTags": {},
        "BackdropImageTags": [],
        "Genres": [],
        "Tags": [],
        "Studios": [],
        "People": [],
        "UserData": {
            "ItemId": id,
            "Key": id,
            "Played": false,
            "IsFavorite": false,
            "PlayCount": 0,
            "PlaybackPositionTicks": 0,
            "PlayedPercentage": null,
            "Rating": null,
            "LastPlayedDate": null,
            "Likes": null,
            "UnplayedItemCount": null,
        },
        "LockedFields": [],
        "LockData": false,
        "ExternalUrls": [],
    })
}

fn root_folder_emby_value(user_id: &str) -> Value {
    let mut value = root_folder_value(user_id);
    if let Some(object) = value.as_object_mut() {
        object.remove("UserId");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{base_item_query_result, query_start_index, root_folder_value};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn root_folder_is_single_base_item_dto() {
        let value = root_folder_value("u1");
        assert_eq!(value["Name"], "Root");
        assert_eq!(value["Type"], "UserRootFolder");
        assert_eq!(value["UserId"], "u1");
        assert_eq!(value["IsFolder"], true);
        assert!(value.get("Items").is_none());
        assert_eq!(value["UserData"]["ItemId"], value["Id"]);
    }

    #[test]
    fn root_folder_defaults_to_request_user() {
        let query = HashMap::new();
        let user_id = crate::jellyfin::auth::query_user_id_or_request(&query, "current");
        assert_eq!(root_folder_value(&user_id)["UserId"], "current");
    }

    #[test]
    fn root_folder_query_user_overrides_request_user() {
        let mut query = HashMap::new();
        query.insert("userId".to_string(), "target".to_string());
        let user_id = crate::jellyfin::auth::query_user_id_or_request(&query, "current");
        assert_eq!(root_folder_value(&user_id)["UserId"], "target");
    }

    #[test]
    fn base_item_query_result_includes_start_index() {
        let value = base_item_query_result(vec![json!({ "Id": "i1" })], 3);
        assert_eq!(value["TotalRecordCount"], 1);
        assert_eq!(value["StartIndex"], 3);
        assert_eq!(value["Items"][0]["Id"], "i1");
    }

    #[test]
    fn query_start_index_reads_jellyfin_casing() {
        let mut query = HashMap::new();
        query.insert("startIndex".to_string(), "4".to_string());
        assert_eq!(query_start_index(&query), 4);
    }
}
