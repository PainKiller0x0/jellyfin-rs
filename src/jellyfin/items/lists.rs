use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    jellyfin::{
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
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match list_media_items(&state.db, &user_id, &query).await {
        Ok((items, total)) => {
            let json_items = super::enrich_episode_list(&state.db, items).await;
            Json(json!({ "Items": json_items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let parent_id = query.get("ParentId").map(String::as_str);
    match latest_media_items(&state.db, &user_id, parent_id).await {
        Ok(items) => Json(
            items
                .into_iter()
                .map(|item| strip_nulls(item.to_jellyfin_json()))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items_root(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let parent_id = query.get("ParentId").map(String::as_str);
    match latest_media_items(&state.db, &user_id, parent_id).await {
        Ok(items) => Json(
            items
                .into_iter()
                .map(|item| strip_nulls(item.to_jellyfin_json()))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn resume_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match resume_media_items(&state.db, &user_id).await {
        Ok(items) => {
            let total = items.len();
            let enriched = super::enrich_resume_items(&state.db, items).await;
            Json(json!({ "Items": enriched, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn items_root(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("userId")
        .or_else(|| query.get("UserId"))
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    Json(root_folder_value(&user_id)).into_response()
}

pub async fn user_items_root(Path(user_id): Path<String>) -> Response {
    Json(root_folder_value(&user_id)).into_response()
}

pub async fn trailers(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .or_else(|| query.get("userId"))
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match list_trailers(&state.db, &user_id, &query).await {
        Ok((items, total)) => media_list_response_with_total(items, total),
        Err(error) => internal_error(error),
    }
}

pub fn media_list_response(items: Vec<MediaItem>) -> Response {
    let total = items.len();
    Json(json!({ "Items": items.into_iter().map(|item| strip_nulls(item.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

pub fn media_list_response_with_total(items: Vec<MediaItem>, total: usize) -> Response {
    Json(json!({ "Items": items.into_iter().map(|item| strip_nulls(item.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

fn root_folder_value(user_id: &str) -> Value {
    let id = stable_text_id(&format!("user-root:{user_id}"));
    json!({
        "Name": "Root",
        "Id": id,
        "Type": "UserRootFolder",
        "UserId": user_id,
        "IsFolder": true,
        "ParentId": null,
        "Path": null,
        "ServerId": "jellyfin-rs",
        "CollectionType": null,
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

#[cfg(test)]
mod tests {
    use super::root_folder_value;

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
}
