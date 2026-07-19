from __future__ import annotations

import copy
import json
import os
import re
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote

import pytest
import requests
from jsonschema import Draft7Validator, FormatChecker, RefResolver
from jsonschema.exceptions import ValidationError


HTTP_METHODS = {"get", "put", "post", "delete", "patch", "head", "options", "trace"}
BASE_DIR = Path(__file__).resolve().parents[2]
SPEC_PATH = Path(os.getenv("EMBY_OPENAPI", BASE_DIR / "docs" / "emby-openapi.json"))
BASE_URL = os.getenv("EMBY_BASE_URL", "http://127.0.0.1:8096").rstrip("/")
USERNAME = os.getenv("EMBY_USERNAME", "admin")
PASSWORD = os.getenv("EMBY_PASSWORD", "123456")
REPORT_PATH = Path(
    os.getenv("EMBY_CONTRACT_REPORT", Path(__file__).with_name("contract-report.jsonl"))
)
INCLUDE_MUTATING = os.getenv("EMBY_CONTRACT_INCLUDE_MUTATING", "false").lower() == "true"
STRICT_ADDITIONAL_PROPERTIES = (
    os.getenv("EMBY_CONTRACT_STRICT_ADDITIONAL_PROPERTIES", "true").lower() == "true"
)

CLIENT = "CodexOpenApiContract"
DEVICE = "Codex"
DEVICE_ID = os.getenv("EMBY_DEVICE_ID", f"codex-openapi-contract-{uuid.uuid4()}")
VERSION = "1.0.0"

USER_SAFE_TAGS = {
    "ArtistsService",
    "AudioService",
    "AlbumsService",
    "BrandingService",
    "FilterService",
    "GameGenresService",
    "GenresService",
    "ImageService",
    "InstantMixService",
    "ItemLookupService",
    "ItemsService",
    "LocalizationService",
    "MusicGenresService",
    "PersonsService",
    "PlaystateService",
    "RemoteImageService",
    "SessionsService",
    "StudiosService",
    "SuggestionsService",
    "SubtitleService",
    "SystemService",
    "TvShowsService",
    "UserLibraryService",
    "UserService",
    "UserViewsService",
    "VideoService",
    "VideosService",
    "YearsService",
}

ADMIN_PATH_PATTERNS = (
    r"^/Auth/Keys",
    r"^/Devices",
    r"^/Dlna",
    r"^/Environment",
    r"^/Library/VirtualFolders",
    r"^/LiveTv",
    r"^/Notifications",
    r"^/Packages",
    r"^/Plugins",
    r"^/ScheduledTasks",
    r"^/Sync",
    r"^/System/(ActivityLog|Configuration|Logs|Ping|Restart|Shutdown|WakeOnLanInfo)",
    r"^/Users/New",
    r"^/Users/\{Id\}/(Configuration|Connect/Link|EasyPassword|Password|Policy)",
    r"^/user_usage_stats",
)

ADMIN_TAGS = {
    "ConfigurationService",
    "DeviceService",
    "DlnaServerService",
    "DlnaService",
    "EnvironmentService",
    "LibraryStructureService",
    "LiveTvService",
    "NotificationsService",
    "PackageService",
    "PluginService",
    "ReportsService",
    "ScheduledTaskService",
    "SyncService",
    "UserActivityAPI",
}

MUTATING_METHODS = {"post", "put", "patch", "delete"}
SAFE_MUTATING_PATHS = {"/Users/AuthenticateByName"}
SIDE_EFFECT_PATH_PATTERNS = (
    r"/FavoriteItems/",
    r"/PlayedItems/",
    r"/PlayingItems/",
    r"/Items/\{Id\}/Rating$",
    r"/Images/",
    r"/DisplayPreferences/",
)


@dataclass(frozen=True)
class Operation:
    method: str
    path: str
    operation_id: str
    tags: tuple[str, ...]
    parameters: tuple[dict[str, Any], ...]
    request_body: dict[str, Any] | None
    responses: dict[str, Any]

    @property
    def test_id(self) -> str:
        return f"{self.method.upper()} {self.path} [{self.operation_id}]"


class MissingContext(RuntimeError):
    pass


def load_spec() -> dict[str, Any]:
    with SPEC_PATH.open(encoding="utf-8") as handle:
        return json.load(handle)


RAW_SPEC = load_spec()
VALIDATION_SPEC = copy.deepcopy(RAW_SPEC)


def bool_env(value: bool) -> str:
    return "true" if value else "false"


def resolve_ref(ref: str, root: dict[str, Any] | None = None) -> Any:
    if not ref.startswith("#/"):
        raise ValueError(f"Only local refs are supported: {ref}")
    node: Any = RAW_SPEC if root is None else root
    for part in ref[2:].split("/"):
        node = node[part.replace("~1", "/").replace("~0", "~")]
    return node


def deref(node: Any) -> Any:
    if isinstance(node, dict) and "$ref" in node:
        return deref(resolve_ref(node["$ref"]))
    return node


def convert_openapi_schema(schema: Any) -> Any:
    if isinstance(schema, list):
        return [convert_openapi_schema(value) for value in schema]
    if not isinstance(schema, dict):
        return schema

    converted = {}
    for key, value in schema.items():
        if key == "nullable":
            continue
        converted[key] = convert_openapi_schema(value)

    if schema.get("nullable"):
        current_type = converted.get("type")
        if isinstance(current_type, str):
            converted["type"] = sorted({current_type, "null"})
        elif isinstance(current_type, list):
            converted["type"] = sorted(set(current_type + ["null"]))
        elif "anyOf" in converted:
            converted["anyOf"].append({"type": "null"})
        elif "oneOf" in converted:
            converted["oneOf"].append({"type": "null"})
        else:
            converted["anyOf"] = [copy.deepcopy(converted), {"type": "null"}]

    if (
        STRICT_ADDITIONAL_PROPERTIES
        and converted.get("type") == "object"
        and "properties" in converted
        and "additionalProperties" not in converted
    ):
        converted["additionalProperties"] = False

    return converted


def convert_validation_spec() -> None:
    components = VALIDATION_SPEC.get("components", {})
    for bucket in ("schemas", "responses"):
        values = components.get(bucket, {})
        for name, value in list(values.items()):
            values[name] = convert_openapi_schema(value)
    for path_item in VALIDATION_SPEC.get("paths", {}).values():
        for method, operation in path_item.items():
            if method.lower() not in HTTP_METHODS:
                continue
            if "requestBody" in operation:
                operation["requestBody"] = convert_openapi_schema(operation["requestBody"])
            if "responses" in operation:
                operation["responses"] = convert_openapi_schema(operation["responses"])


convert_validation_spec()


def all_operations() -> list[Operation]:
    operations: list[Operation] = []
    for path, path_item in RAW_SPEC.get("paths", {}).items():
        path_params = tuple(path_item.get("parameters", ()))
        for method, operation in path_item.items():
            if method.lower() not in HTTP_METHODS:
                continue
            parameters = path_params + tuple(operation.get("parameters", ()))
            operation_id = operation.get("operationId") or f"{method}_{path}"
            operations.append(
                Operation(
                    method=method.lower(),
                    path=path,
                    operation_id=operation_id,
                    tags=tuple(operation.get("tags", ())),
                    parameters=parameters,
                    request_body=operation.get("requestBody"),
                    responses=operation.get("responses", {}),
                )
            )
    return operations


def is_admin_operation(operation: Operation) -> bool:
    if set(operation.tags) & ADMIN_TAGS:
        return True
    return any(re.search(pattern, operation.path) for pattern in ADMIN_PATH_PATTERNS)


def is_side_effect_operation(operation: Operation) -> bool:
    if operation.method not in MUTATING_METHODS:
        return False
    if operation.path in SAFE_MUTATING_PATHS:
        return False
    return any(re.search(pattern, operation.path) for pattern in SIDE_EFFECT_PATH_PATTERNS)


def is_user_operation(operation: Operation) -> bool:
    tags = set(operation.tags)
    if is_admin_operation(operation):
        return False
    if not (tags & USER_SAFE_TAGS):
        return False
    if operation.method in MUTATING_METHODS and operation.path not in SAFE_MUTATING_PATHS:
        return INCLUDE_MUTATING and is_side_effect_operation(operation)
    if operation.path == "/Users":
        return False
    if operation.path.startswith("/Users/") and any(
        blocked in operation.path
        for blocked in ("/Password", "/EasyPassword", "/Policy", "/Configuration", "/Connect/Link")
    ):
        return False
    return True


SELECTED_OPERATIONS = [operation for operation in all_operations() if is_user_operation(operation)]


def optional_parameters(operation: Operation) -> list[dict[str, Any]]:
    return [param for param in operation.parameters if not param.get("required")]


OPTIONAL_PARAMETER_CASES = [
    (operation, param)
    for operation in SELECTED_OPERATIONS
    for param in optional_parameters(operation)
]


def make_auth_header(token: str | None = None, user_id: str | None = None) -> str:
    parts = {
        "UserId": user_id,
        "Client": CLIENT,
        "Device": DEVICE,
        "DeviceId": DEVICE_ID,
        "Version": VERSION,
        "Token": token,
    }
    return "Emby " + ", ".join(
        f'{key}="{value}"' for key, value in parts.items() if value is not None
    )


@pytest.fixture(scope="session")
def http() -> requests.Session:
    session = requests.Session()
    session.headers.update(
        {
            "Accept": "application/json",
            "X-Emby-Authorization": make_auth_header(),
        }
    )
    return session


@pytest.fixture(scope="session")
def auth(http: requests.Session) -> dict[str, Any]:
    response = http.post(
        f"{BASE_URL}/Users/AuthenticateByName",
        json={"Username": USERNAME, "Pw": PASSWORD},
        timeout=20,
    )
    assert response.status_code == 200, (
        "Authentication failed against /Users/AuthenticateByName\n"
        f"expected: 200\nactual: {response.status_code}\nbody: {response.text[:1000]}"
    )
    payload = response.json()
    user_id = payload["User"]["Id"]
    token = payload["AccessToken"]
    http.headers.update(
        {
            "X-Emby-Authorization": make_auth_header(token=token, user_id=user_id),
            "X-Emby-Token": token,
        }
    )
    return {
        "token": token,
        "user_id": user_id,
        "server_id": payload.get("ServerId"),
        "session_id": (payload.get("SessionInfo") or {}).get("Id"),
        "auth_payload": payload,
    }


@pytest.fixture(scope="session")
def context(http: requests.Session, auth: dict[str, Any]) -> dict[str, Any]:
    values: dict[str, Any] = {
        "base_url": BASE_URL,
        "username": USERNAME,
        "password": PASSWORD,
        "token": auth["token"],
        "user_id": auth["user_id"],
        "server_id": auth.get("server_id"),
        "session_id": auth.get("session_id"),
        "device_id": DEVICE_ID,
        "item_id": None,
        "folder_id": None,
        "media_item_id": None,
        "video_id": None,
        "media_source_id": None,
        "container": None,
        "image_item_id": None,
        "image_type": "Primary",
        "user_image_type": None,
        "view_id": None,
        "artist_name": None,
        "genre_name": None,
        "game_genre_name": None,
        "music_genre_name": None,
        "person_name": None,
        "studio_name": None,
        "item_path": None,
        "subtitle_id": None,
    }
    auth_user = auth.get("auth_payload", {}).get("User") or {}
    if auth_user.get("PrimaryImageTag"):
        values["user_image_type"] = "Primary"

    items = get_json(
        http,
        f"/Users/{quote(auth['user_id'], safe='')}/Items",
        params={
            "Recursive": "true",
            "Limit": "50",
            "Fields": "MediaSources,ImageTags,UserData,Genres,People,Studios,ProviderIds",
        },
    )
    for item in items.get("Items", []):
        values["item_id"] = values["item_id"] or item.get("Id")
        values["item_path"] = values["item_path"] or item.get("Path")
        if item.get("IsFolder"):
            values["folder_id"] = values["folder_id"] or item.get("Id")
        if item.get("Type") == "Video" or item.get("MediaType") == "Video":
            values["video_id"] = values["video_id"] or item.get("Id")
        image_tags = item.get("ImageTags") or {}
        if image_tags and not values["image_item_id"]:
            values["image_item_id"] = item.get("Id")
            values["image_type"] = next(iter(image_tags.keys()), "Primary")
        media_sources = item.get("MediaSources") or []
        if media_sources:
            values["media_item_id"] = values["media_item_id"] or item.get("Id")
            values["media_source_id"] = values["media_source_id"] or media_sources[0].get("Id")
            values["container"] = values["container"] or media_sources[0].get("Container")
        if item.get("Artists"):
            values["artist_name"] = values["artist_name"] or item["Artists"][0]
        if item.get("Genres"):
            values["genre_name"] = values["genre_name"] or item["Genres"][0]
        for person in item.get("People") or []:
            values["person_name"] = values["person_name"] or person.get("Name")
        for studio in item.get("Studios") or []:
            values["studio_name"] = values["studio_name"] or studio.get("Name")

    views = get_json(
        http,
        f"/Users/{quote(auth['user_id'], safe='')}/Views",
        params={"IncludeExternalContent": "true"},
    )
    for view in views.get("Items", []):
        values["view_id"] = values["view_id"] or view.get("Id")
        if not values["folder_id"]:
            values["folder_id"] = view.get("Id")

    sessions = get_json(http, "/Sessions")
    for session in sessions if isinstance(sessions, list) else []:
        values["session_id"] = values["session_id"] or session.get("Id")

    for path, key in (
        ("/Artists", "artist_name"),
        ("/Genres", "genre_name"),
        ("/GameGenres", "game_genre_name"),
        ("/MusicGenres", "music_genre_name"),
        ("/Persons", "person_name"),
        ("/Studios", "studio_name"),
    ):
        payload = get_json(http, path, params={"Limit": "1"})
        for item in payload.get("Items", []) if isinstance(payload, dict) else []:
            values[key] = values[key] or item.get("Name")

    values["item_id"] = values["item_id"] or values["folder_id"] or values["video_id"]
    values["media_item_id"] = values["media_item_id"] or values["video_id"] or values["item_id"]
    values["video_id"] = values["video_id"] or values["media_item_id"] or values["item_id"]
    values["image_item_id"] = values["image_item_id"] or values["item_id"]
    values["media_source_id"] = values["media_source_id"] or values["media_item_id"] or values["item_id"]
    return values


def get_json(http: requests.Session, path: str, params: dict[str, Any] | None = None) -> Any:
    response = http.get(f"{BASE_URL}{path}", params=params or {}, timeout=20)
    if response.status_code != 200:
        return {}
    try:
        return response.json()
    except ValueError:
        return {}


def schema_for_parameter(param: dict[str, Any]) -> dict[str, Any]:
    return deref(param.get("schema") or {})


def schema_sample(schema: dict[str, Any], *, name: str = "") -> Any:
    schema = deref(schema)
    if "enum" in schema:
        return schema["enum"][0]
    if "default" in schema:
        return schema["default"]
    if "example" in schema:
        return schema["example"]
    if "examples" in schema and schema["examples"]:
        return schema["examples"][0]
    schema_type = schema.get("type")
    schema_format = schema.get("format")
    if schema_type == "array":
        return [schema_sample(schema.get("items") or {"type": "string"}, name=name)]
    if schema_type == "boolean":
        return True
    if schema_type == "integer":
        return 0 if "Index" in name else 1
    if schema_type == "number":
        return 1.0
    if schema_type == "object":
        return request_body_sample(schema)
    if schema_format == "guid":
        return "00000000-0000-0000-0000-000000000000"
    if schema_format == "date-time" or "Date" in name:
        return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    if "Color" in name:
        return "000000"
    if "SortOrder" in name:
        return "Ascending"
    if "PlayMethod" in name:
        return "DirectPlay"
    if "RepeatMode" in name:
        return "RepeatNone"
    return "test"


def request_body_sample(schema: dict[str, Any]) -> dict[str, Any]:
    schema = deref(schema)
    body: dict[str, Any] = {}
    for subschema_key in ("allOf", "anyOf", "oneOf"):
        for subschema in schema.get(subschema_key, [])[:1]:
            body.update(request_body_sample(subschema))
    for name, prop_schema in (schema.get("properties") or {}).items():
        if prop_schema.get("readOnly"):
            continue
        if name.lower() in {"username", "name"}:
            body[name] = USERNAME if name.lower() == "username" else "Codex Contract"
        elif name.lower() in {"pw", "password", "currentpw", "currentpassword"}:
            body[name] = PASSWORD
        else:
            body[name] = schema_sample(prop_schema, name=name)
    return body


def semantic_value(operation: Operation, param: dict[str, Any], context: dict[str, Any]) -> Any:
    name = param["name"]
    location = param.get("in")
    schema = schema_for_parameter(param)

    if location == "header" and name.lower() in {"x-emby-authorization", "authorization"}:
        return make_auth_header(token=context["token"], user_id=context["user_id"])
    if name == "UserId":
        return require_context(context, "user_id", operation, param)
    if name == "Id":
        return id_value_for_path(operation, context, param)
    if name == "Type" and "Images" in operation.path:
        if operation.path.startswith("/Users/{Id}/Images"):
            return require_context(context, "user_image_type", operation, param)
        return context.get("image_type") or schema_sample(schema, name=name)
    if name == "Index":
        return 0
    if name == "Name":
        if operation.path.startswith("/Artists"):
            return require_context(context, "artist_name", operation, param)
        if operation.path.startswith("/Genres") or operation.path.startswith("/MusicGenres"):
            key = "music_genre_name" if operation.path.startswith("/MusicGenres") else "genre_name"
            return require_context(context, key, operation, param)
        if operation.path.startswith("/GameGenres"):
            return require_context(context, "game_genre_name", operation, param)
        if operation.path.startswith("/Persons"):
            return require_context(context, "person_name", operation, param)
        if operation.path.startswith("/Studios"):
            return require_context(context, "studio_name", operation, param)
    if name in {"ItemId", "ItemIds", "Ids"}:
        return require_context(context, "item_id", operation, param)
    if name in {"MediaSourceId", "MediaSourceIds"}:
        return require_context(context, "media_source_id", operation, param)
    if name == "ParentId":
        return context.get("folder_id") or context.get("view_id") or context.get("item_id")
    if name in {"ControllableByUserId", "UserIds"}:
        return require_context(context, "user_id", operation, param)
    if name == "DeviceId":
        return context["device_id"]
    if name == "Container":
        return context.get("container") or "mkv"
    if name == "PlaySessionId":
        return context.get("session_id") or "codex-contract-play-session"
    if name == "SessionId":
        return require_context(context, "session_id", operation, param)
    if name in {"IncludeItemTypes", "ExcludeItemTypes"}:
        return "Movie"
    if name in {"Fields", "EnableImageTypes", "ImageTypes"}:
        return "PrimaryImageAspectRatio,MediaSources,UserData,Overview,Genres,People"
    if name == "SortBy":
        return "SortName"
    if name == "SortOrder":
        return "Ascending"
    if name == "Recursive":
        return True
    if name == "IncludeExternalContent":
        return True
    if name == "Limit":
        return 2
    if name == "StartIndex":
        return 0
    if name == "DatePlayed":
        return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    if name == "NextMediaType":
        return "Video"
    if name == "PlayMethod":
        return "DirectPlay"
    if name == "RepeatMode":
        return "RepeatNone"
    if name == "LiveStreamId":
        return "codex-contract-live-stream"
    if name == "Tag":
        return "test"
    if name == "Format":
        if "/Subtitles/" in operation.path:
            return "srt"
        return "jpg"
    if name == "Width" or name == "Height" or name in {"MaxWidth", "MaxHeight"}:
        return 64
    if name == "Quality":
        return 90
    if name in {"IsPlayed", "IsFavorite", "IsFolder", "EnableImages", "EnableUserData"}:
        return False
    if name in {"CanSeek", "IsPaused", "IsMuted", "GroupItems", "AddPlayedIndicator"}:
        return True
    if name == "Language":
        return "eng"
    if name == "Path":
        return require_context(context, "item_path", operation, param)
    return schema_sample(schema, name=name)


def id_value_for_path(operation: Operation, context: dict[str, Any], param: dict[str, Any]) -> str:
    path = operation.path
    if path.startswith("/Users/{Id}"):
        if path.startswith("/Users/{Id}/Images"):
            require_context(context, "user_image_type", operation, param)
        return require_context(context, "user_id", operation, param)
    if path.startswith("/Providers/Subtitles/Subtitles/{Id}"):
        return require_context(context, "subtitle_id", operation, param)
    if path.startswith("/Sessions/{Id}"):
        return require_context(context, "session_id", operation, param)
    if path.startswith("/Videos/{Id}") or path.startswith("/Audio/{Id}"):
        return require_context(context, "media_item_id", operation, param)
    if "/Images/" in path:
        return require_context(context, "image_item_id", operation, param)
    if "/Items/{Id}" in path or path.startswith("/Items/{Id}"):
        return require_context(context, "item_id", operation, param)
    return require_context(context, "item_id", operation, param)


def require_context(
    context: dict[str, Any], key: str, operation: Operation, param: dict[str, Any]
) -> Any:
    value = context.get(key)
    if value in (None, ""):
        raise MissingContext(
            f"{operation.test_id} needs context value {key!r} for parameter {param['name']!r}"
        )
    return value


def serialize_param(value: Any) -> str:
    if isinstance(value, bool):
        return bool_env(value)
    if isinstance(value, (list, tuple)):
        return ",".join(serialize_param(item) for item in value)
    return str(value)


def build_request(
    operation: Operation,
    context: dict[str, Any],
    *,
    optional_param: dict[str, Any] | None = None,
) -> dict[str, Any]:
    path = operation.path
    params: dict[str, str] = {}
    headers: dict[str, str] = {}
    json_body: Any = None

    selected_params = []
    for param in operation.parameters:
        if param.get("required") or (optional_param and param["name"] == optional_param["name"]):
            selected_params.append(param)

    for param in selected_params:
        value = semantic_value(operation, param, context)
        location = param.get("in")
        if location == "path":
            path = path.replace("{" + param["name"] + "}", quote(serialize_param(value), safe=""))
        elif location == "query":
            params[param["name"]] = serialize_param(value)
        elif location == "header":
            headers[param["name"]] = serialize_param(value)
        elif location == "cookie":
            headers["Cookie"] = f"{param['name']}={serialize_param(value)}"

    if "{" in path:
        raise MissingContext(f"{operation.test_id} still has unresolved path variables: {path}")

    if operation.request_body:
        json_schema = first_json_schema(operation.request_body)
        if operation.path == "/Users/AuthenticateByName":
            json_body = {"Username": USERNAME, "Pw": PASSWORD}
        elif json_schema:
            json_body = request_body_sample(json_schema)

    return {
        "url": f"{BASE_URL}{path}",
        "params": params,
        "headers": headers,
        "json": json_body,
    }


def first_json_schema(openapi_message: dict[str, Any], *, json_only: bool = False) -> dict[str, Any] | None:
    content = deref(openapi_message).get("content") or {}
    for content_type in ("application/json", "text/json", "*/*"):
        schema = (content.get(content_type) or {}).get("schema")
        if schema:
            return schema
    if json_only:
        return None
    for media in content.values():
        schema = media.get("schema")
        if schema:
            return schema
    return None


def documented_content_types(openapi_message: dict[str, Any]) -> list[str]:
    return list((deref(openapi_message).get("content") or {}).keys())


def response_for_status(operation: Operation, status_code: int) -> dict[str, Any] | None:
    responses = operation.responses
    status = str(status_code)
    if status in responses:
        return deref(responses[status])
    if 500 <= status_code <= 599 and "5XX" in responses:
        return deref(responses["5XX"])
    default = responses.get("default")
    return deref(default) if default else None


def success_statuses(operation: Operation) -> list[int]:
    statuses = []
    for status in operation.responses:
        if status.isdigit() and 200 <= int(status) <= 299:
            statuses.append(int(status))
    return sorted(statuses)


def expected_status_summary(operation: Operation) -> str:
    statuses = success_statuses(operation)
    if statuses:
        return ", ".join(str(status) for status in statuses)
    return ", ".join(operation.responses.keys())


def validation_schema_for_response(operation: Operation, status_code: int) -> dict[str, Any] | None:
    response_spec = response_for_status(operation, status_code)
    if not response_spec:
        return None
    return first_json_schema(response_spec, json_only=True)


def validator_for(schema: dict[str, Any]) -> Draft7Validator:
    converted_schema = convert_openapi_schema(schema)
    return Draft7Validator(
        converted_schema,
        resolver=RefResolver.from_schema(VALIDATION_SPEC),
        format_checker=FormatChecker(),
    )


def compact_json(value: Any, limit: int = 2000) -> str:
    text = json.dumps(value, ensure_ascii=False, sort_keys=True)
    if len(text) > limit:
        return text[:limit] + "...<truncated>"
    return text


def error_summary(errors: list[ValidationError], limit: int = 20) -> list[dict[str, Any]]:
    summary = []
    for error in sorted(errors, key=lambda item: list(item.path))[:limit]:
        summary.append(
            {
                "path": list(error.absolute_path),
                "message": error.message,
                "schema_path": list(error.absolute_schema_path),
                "actual": error.instance,
            }
        )
    return summary


def content_type_matches(actual: str, expected_values: list[str]) -> bool:
    actual_main = actual.split(";")[0].strip().lower()
    for expected in expected_values:
        expected_main = expected.split(";")[0].strip().lower()
        if expected_main in {"*/*", ""}:
            return True
        if expected_main.endswith("/*") and actual_main.startswith(expected_main[:-1]):
            return True
        if actual_main == expected_main:
            return True
    return False


def read_body_sample(response: requests.Response, limit: int = 2000) -> tuple[str, Any]:
    data = response.raw.read(limit + 1, decode_content=True)
    suffix = "...<truncated>" if len(data) > limit else ""
    data = data[:limit]
    try:
        return data.decode(response.encoding or "utf-8", errors="replace") + suffix, data
    finally:
        response.close()


def write_report(record: dict[str, Any]) -> None:
    REPORT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with REPORT_PATH.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")


def call_and_validate(
    http: requests.Session,
    operation: Operation,
    request_kwargs: dict[str, Any],
    *,
    case: str,
    parameter: str | None = None,
) -> None:
    response = http.request(operation.method.upper(), timeout=30, stream=True, **request_kwargs)
    content_type = response.headers.get("content-type", "")
    response_spec = response_for_status(operation, response.status_code)
    schema = validation_schema_for_response(operation, response.status_code)
    response_text = ""
    actual_payload: Any = None
    parse_error = None

    if operation.method == "head":
        response.close()
    elif schema is not None:
        response_text = response.text
        response.close()
    else:
        response_text, actual_payload = read_body_sample(response)

    if schema is not None and response_text.strip():
        try:
            actual_payload = response.json()
        except ValueError as exc:
            parse_error = str(exc)
            actual_payload = response_text[:2000]

    record = {
        "case": case,
        "parameter": parameter,
        "operation": operation.operation_id,
        "method": operation.method.upper(),
        "path": operation.path,
        "url": request_kwargs["url"],
        "query": request_kwargs.get("params") or {},
        "expected_success_status": expected_status_summary(operation),
        "actual_status": response.status_code,
        "content_type": content_type,
        "strict_additional_properties": STRICT_ADDITIONAL_PROPERTIES,
        "body_sample": actual_payload if isinstance(actual_payload, (dict, list)) else response_text[:1000],
    }

    try:
        statuses = success_statuses(operation)
        assert statuses, f"{operation.test_id} has no documented 2xx response to validate"
        assert response.status_code in statuses, (
            f"{operation.test_id}\n"
            f"expected success status: {expected_status_summary(operation)}\n"
            f"actual status: {response.status_code}\n"
            f"query: {request_kwargs.get('params') or {}}\n"
            f"body: {response_text[:2000]}"
        )

        if schema is None:
            content_types = documented_content_types(response_spec or {})
            if content_types:
                assert content_type_matches(content_type, content_types), (
                    f"{operation.test_id}\n"
                    f"expected content-type: {content_types}\n"
                    f"actual content-type: {content_type}\n"
                    f"body sample: {response_text[:1000]}"
                )
            else:
                if response_text.strip():
                    assert not response_text.strip(), (
                        f"{operation.test_id}\n"
                        "expected: no documented response body\n"
                        f"actual content-type: {content_type}\n"
                        f"actual body: {response_text[:2000]}"
                    )
            record["schema_errors"] = []
            record["passed"] = True
            return

        assert "json" in content_type.lower(), (
            f"{operation.test_id}\n"
            "expected content-type: application/json\n"
            f"actual content-type: {content_type}\n"
            f"body: {response_text[:1000]}"
        )
        assert parse_error is None, (
            f"{operation.test_id}\n"
            f"expected JSON body matching OpenAPI schema\n"
            f"actual JSON parse error: {parse_error}\n"
            f"body: {response_text[:1000]}"
        )

        errors = list(validator_for(schema).iter_errors(actual_payload))
        record["schema_errors"] = error_summary(errors)
        assert not errors, (
            f"{operation.test_id}\n"
            f"schema mismatch for {case}"
            + (f" parameter {parameter}" if parameter else "")
            + "\n"
            f"query: {request_kwargs.get('params') or {}}\n"
            f"expected schema: {compact_json(schema, limit=3000)}\n"
            f"actual body: {compact_json(actual_payload, limit=3000)}\n"
            f"errors: {compact_json(error_summary(errors), limit=4000)}"
        )
        record["passed"] = True
    except AssertionError as exc:
        record["passed"] = False
        record["failure"] = str(exc)[:4000]
        raise
    finally:
        write_report(record)


@pytest.fixture(scope="session", autouse=True)
def reset_report() -> None:
    REPORT_PATH.parent.mkdir(parents=True, exist_ok=True)
    REPORT_PATH.write_text("", encoding="utf-8")


def operation_case_id(operation: Operation) -> str:
    return f"{operation.method.upper()} {operation.path}"


def parameter_case_id(case: tuple[Operation, dict[str, Any]]) -> str:
    operation, param = case
    return f"{operation.method.upper()} {operation.path} :: {param.get('in')} {param['name']}"


@pytest.mark.parametrize("operation", SELECTED_OPERATIONS, ids=operation_case_id)
def test_user_operation_required_contract(
    http: requests.Session, context: dict[str, Any], operation: Operation
) -> None:
    try:
        request_kwargs = build_request(operation, context)
    except MissingContext as exc:
        pytest.skip(str(exc))
    call_and_validate(http, operation, request_kwargs, case="required-parameters")


@pytest.mark.parametrize("case", OPTIONAL_PARAMETER_CASES, ids=parameter_case_id)
def test_user_operation_optional_parameter_contract(
    http: requests.Session, context: dict[str, Any], case: tuple[Operation, dict[str, Any]]
) -> None:
    operation, param = case
    try:
        request_kwargs = build_request(operation, context, optional_param=param)
    except MissingContext as exc:
        pytest.skip(str(exc))
    call_and_validate(
        http,
        operation,
        request_kwargs,
        case="single-optional-parameter",
        parameter=f"{param.get('in')}:{param['name']}",
    )


def test_selected_operation_parameter_strategy_covers_every_parameter(
    context: dict[str, Any],
) -> None:
    missing: list[str] = []
    for operation in SELECTED_OPERATIONS:
        for param in operation.parameters:
            try:
                semantic_value(operation, param, context)
            except MissingContext:
                continue
            except Exception as exc:  # noqa: BLE001 - this is a coverage diagnostic.
                missing.append(f"{operation.test_id} {param.get('in')}:{param['name']} -> {exc}")
    assert not missing, "Parameter strategy failed for documented parameters:\n" + "\n".join(missing)


def test_selection_summary_is_not_empty() -> None:
    assert SELECTED_OPERATIONS, "No user operations were selected from docs/emby-openapi.json"
