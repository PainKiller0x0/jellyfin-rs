#!/usr/bin/env python3
import argparse
import json
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import quote, urljoin

import httpx


CLIENT = "Codex OpenAPI Compat"
DEVICE = "Docker OpenAPI Probe"
DEVICE_ID = "codex-openapi-compat"
VERSION = "0.0.0"

FAKE_UUID = "00000000-0000-0000-0000-000000000001"
FAKE_UUID_2 = "00000000-0000-0000-0000-000000000002"

HTTP_METHODS = {"get", "post", "put", "delete", "patch", "head", "options"}
MUTATING_METHODS = {"POST", "PUT", "PATCH", "DELETE"}

SAFE_AUTH_MUTATIONS = {
    "AuthenticateUserByName",
    "GetPostedPlaybackInfo",
    "PostPingSystem",
    "ReportPlaybackStart",
    "ReportPlaybackProgress",
    "ReportPlaybackStopped",
    "PingPlaybackSession",
    "PostCapabilities",
    "PostFullCapabilities",
    "ReportViewing",
    "MarkFavoriteItem",
    "UnmarkFavoriteItem",
    "MarkPlayedItem",
    "MarkUnplayedItem",
}

OPTIONAL_QUERY_VALUES = {
    "userId",
    "limit",
    "recursive",
    "fields",
    "providerName",
}


@dataclass
class Operation:
    method: str
    path: str
    operation_id: str
    tags: list[str]
    parameters: list[dict[str, Any]]
    request_body: dict[str, Any] | None
    responses: set[str]

    @property
    def label(self) -> str:
        return f"{self.method} {self.path} ({self.operation_id})"

    @property
    def is_quick_connect(self) -> bool:
        text = " ".join([self.path, self.operation_id, *self.tags]).lower()
        return "quickconnect" in text or "quick connect" in text

    @property
    def is_mutating(self) -> bool:
        return self.method in MUTATING_METHODS


@dataclass
class RoutePattern:
    path: str
    methods: set[str]
    regex: re.Pattern[str]

    def matches(self, method: str, concrete_path: str) -> bool:
        method_ok = method in self.methods or (method == "HEAD" and "GET" in self.methods)
        return method_ok and self.regex.fullmatch(concrete_path) is not None


@dataclass
class Fixtures:
    token: str = ""
    user_id: str = FAKE_UUID
    item_id: str = FAKE_UUID
    series_id: str = FAKE_UUID
    season_id: str = FAKE_UUID
    media_source_id: str = FAKE_UUID
    session_id: str = FAKE_UUID
    library_id: str = FAKE_UUID
    artist_name: str = "OpenAPI Artist"
    genre_name: str = "OpenAPI Genre"
    music_genre_name: str = "OpenAPI Genre"
    person_name: str = "OpenAPI Person"
    studio_name: str = "OpenAPI Studio"
    log_name: str = "jellyfin-rs.log"
    container: str = "mkv"
    play_session_id: str = "codex-openapi-play-session"


@dataclass
class ProbeResult:
    operation: Operation
    status: int | None
    ok: bool
    mode: str
    detail: str = ""


class JellyfinClient:
    def __init__(self, base_url: str, username: str, password: str, timeout: float) -> None:
        self.base_url = base_url.rstrip("/") + "/"
        self.username = username
        self.password = password
        self.http = httpx.Client(
            timeout=httpx.Timeout(timeout, connect=min(timeout, 5.0)),
            follow_redirects=False,
        )
        self.fixtures = Fixtures()

    def url(self, path: str) -> str:
        return urljoin(self.base_url, path.lstrip("/"))

    def auth_header(self, include_token: bool = True) -> str:
        parts = [
            f'UserId="{self.fixtures.user_id}"',
            f'Client="{CLIENT}"',
            f'Device="{DEVICE}"',
            f'DeviceId="{DEVICE_ID}"',
            f'Version="{VERSION}"',
        ]
        if include_token and self.fixtures.token:
            parts.append(f'Token="{self.fixtures.token}"')
        return "MediaBrowser " + ", ".join(parts)

    def headers(self, authenticated: bool = True, content_type: str | None = None) -> dict[str, str]:
        headers = {
            "Accept": "application/json",
            "User-Agent": f"{CLIENT}/{VERSION}",
        }
        if authenticated:
            headers["Authorization"] = self.auth_header()
        if content_type:
            headers["Content-Type"] = content_type
        return headers

    def login(self) -> None:
        response = self.http.post(
            self.url("/Users/AuthenticateByName"),
            headers=self.headers(authenticated=False, content_type="application/json")
            | {"Authorization": self.auth_header(include_token=False)},
            json={"Username": self.username, "Pw": self.password},
        )
        response.raise_for_status()
        data = response.json()
        self.fixtures.token = data["AccessToken"]
        self.fixtures.user_id = data["User"]["Id"]
        session_id = data.get("SessionInfo", {}).get("Id")
        if session_id:
            self.fixtures.session_id = session_id

    def get_json(self, path: str, **params: Any) -> Any:
        response = self.http.get(
            self.url(path),
            headers=self.headers(),
            params={key: str(value) for key, value in params.items() if value not in (None, "")},
        )
        if response.status_code >= 400:
            return None
        try:
            return response.json()
        except json.JSONDecodeError:
            return None

    def discover_fixtures(self) -> None:
        items = self.get_json(
            f"/Users/{self.fixtures.user_id}/Items",
            Recursive="true",
            IncludeItemTypes="Movie,Episode,Video,Series,Season,Audio",
            Fields="MediaSources",
            Limit=200,
        )
        if isinstance(items, dict):
            rows = items.get("Items") or []
            for item in rows:
                item_type = str(item.get("Type", "")).lower()
                if item_type in {"movie", "episode", "video", "audio"} and self.fixtures.item_id == FAKE_UUID:
                    self.fixtures.item_id = str(item.get("Id") or FAKE_UUID)
                    sources = item.get("MediaSources") or []
                    if sources and isinstance(sources[0], dict):
                        self.fixtures.media_source_id = str(
                            sources[0].get("Id") or self.fixtures.item_id
                        )
                        container = sources[0].get("Container")
                        if isinstance(container, str) and container:
                            self.fixtures.container = container.split(",", 1)[0]
                if item_type == "series" and self.fixtures.series_id == FAKE_UUID:
                    self.fixtures.series_id = str(item.get("Id") or FAKE_UUID)
                if item_type == "season" and self.fixtures.season_id == FAKE_UUID:
                    self.fixtures.season_id = str(item.get("Id") or FAKE_UUID)

        playback = self.get_json(
            f"/Items/{self.fixtures.item_id}/PlaybackInfo",
            UserId=self.fixtures.user_id,
            IsPlayback="true",
            EnableDirectPlay="true",
            EnableDirectStream="true",
            MaxStreamingBitrate=2147483647,
        )
        if isinstance(playback, dict):
            sources = playback.get("MediaSources") or []
            if sources and isinstance(sources[0], dict):
                self.fixtures.media_source_id = str(sources[0].get("Id") or self.fixtures.media_source_id)
                container = sources[0].get("Container")
                if isinstance(container, str) and container:
                    self.fixtures.container = container.split(",", 1)[0]

        sessions = self.get_json("/Sessions")
        if isinstance(sessions, list) and sessions:
            session_id = sessions[0].get("Id")
            if session_id:
                self.fixtures.session_id = str(session_id)

        folders = self.get_json("/Library/MediaFolders")
        if isinstance(folders, dict):
            rows = folders.get("Items") or []
            if rows and isinstance(rows[0], dict):
                self.fixtures.library_id = str(rows[0].get("Id") or FAKE_UUID)

        self._first_named_fixture("/Artists", "artist_name")
        self._first_named_fixture("/Genres", "genre_name")
        self._first_named_fixture("/MusicGenres", "music_genre_name")
        self._first_named_fixture("/Persons", "person_name")
        self._first_named_fixture("/Studios", "studio_name")

        logs = self.get_json("/System/Logs")
        if isinstance(logs, list) and logs and isinstance(logs[0], dict):
            name = logs[0].get("Name")
            if isinstance(name, str) and name:
                self.fixtures.log_name = name

    def _first_named_fixture(self, path: str, attr: str) -> None:
        data = self.get_json(path, userId=self.fixtures.user_id, Limit=1)
        if isinstance(data, dict):
            rows = data.get("Items") or []
        elif isinstance(data, list):
            rows = data
        else:
            rows = []
        if rows and isinstance(rows[0], dict):
            name = rows[0].get("Name")
            if isinstance(name, str) and name:
                setattr(self.fixtures, attr, name)


def load_operations(spec_path: Path) -> list[Operation]:
    spec = json.loads(spec_path.read_text())
    operations: list[Operation] = []
    for path, methods in spec.get("paths", {}).items():
        for method, op in methods.items():
            if method.lower() not in HTTP_METHODS:
                continue
            operations.append(
                Operation(
                    method=method.upper(),
                    path=path,
                    operation_id=op.get("operationId") or f"{method.upper()} {path}",
                    tags=list(op.get("tags") or []),
                    parameters=list(op.get("parameters") or []),
                    request_body=op.get("requestBody"),
                    responses=set(op.get("responses") or {}),
                )
            )
    return operations


def extract_axum_routes(source_path: Path) -> list[RoutePattern]:
    source = source_path.read_text()
    routes: list[RoutePattern] = []
    index = 0
    while True:
        start = source.find(".route(", index)
        if start == -1:
            break
        path_start = start + len(".route(")
        while path_start < len(source) and source[path_start].isspace():
            path_start += 1
        if path_start >= len(source) or source[path_start] != '"':
            index = path_start
            continue
        path_end = path_start + 1
        while path_end < len(source):
            if source[path_end] == '"' and source[path_end - 1] != "\\":
                break
            path_end += 1
        path = source[path_start + 1 : path_end]
        call_start = start + len(".route")
        call_end = find_balanced_call_end(source, call_start)
        expression = source[start:call_end]
        methods = route_methods(expression)
        routes.append(RoutePattern(path=path, methods=methods, regex=axum_path_regex(path)))
        index = call_end
    return routes


def find_balanced_call_end(source: str, start: int) -> int:
    depth = 0
    in_string = False
    escaped = False
    for pos in range(start, len(source)):
        char = source[pos]
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return pos + 1
    return len(source)


def route_methods(expression: str) -> set[str]:
    methods: set[str] = set()
    for function, method in [
        ("get", "GET"),
        ("post", "POST"),
        ("delete", "DELETE"),
        ("put", "PUT"),
        ("patch", "PATCH"),
        ("head", "HEAD"),
        ("options", "OPTIONS"),
    ]:
        if re.search(rf"(?<![A-Za-z0-9_]){function}\s*\(", expression) or re.search(
            rf"\.{function}\s*\(", expression
        ):
            methods.add(method)
    return methods or {"?"}


def axum_path_regex(path: str) -> re.Pattern[str]:
    pattern = ""
    pos = 0
    while pos < len(path):
        char = path[pos]
        if char == "{":
            end = path.index("}", pos)
            pattern += r"[^/]+"
            pos = end + 1
        else:
            pattern += re.escape(char)
            pos += 1
    return re.compile(pattern)


def static_route_check(operations: list[Operation], routes: list[RoutePattern], fixtures: Fixtures) -> list[str]:
    missing: list[str] = []
    for operation in operations:
        if operation.is_quick_connect:
            continue
        concrete_path = render_path(operation.path, operation, fixtures)
        if not any(route.matches(operation.method, concrete_path) for route in routes):
            missing.append(operation.label)
    return missing


def render_path(template: str, operation: Operation, fixtures: Fixtures) -> str:
    def replace(match: re.Match[str]) -> str:
        value = path_value(match.group(1), operation, fixtures)
        return quote(str(value), safe="")

    return re.sub(r"\{([^}]+)\}", replace, template)


def path_value(name: str, operation: Operation, fixtures: Fixtures) -> str | int:
    key = name.lower()
    if key in {"userid", "user_id"}:
        return fixtures.user_id
    if key in {"itemid", "routeitemid", "videoid"}:
        if operation.method in {"GET", "HEAD"} or operation.operation_id in SAFE_AUTH_MUTATIONS:
            return fixtures.item_id
        return FAKE_UUID
    if key == "seriesid":
        return fixtures.series_id
    if key == "media_source_id" or key in {"mediasourceid", "routemediasourceid"}:
        return fixtures.media_source_id
    if key in {"sessionid"}:
        return fixtures.session_id
    if key == "collectionid":
        return FAKE_UUID
    if key == "playlistid":
        return FAKE_UUID
    if key == "channelid":
        return FAKE_UUID
    if key == "displaypreferencesid":
        return "home"
    if key == "key":
        return "openapi-compat"
    if key == "name":
        return name_value_for_path(operation.path, fixtures)
    if key == "genrename":
        return fixtures.genre_name
    if key == "imagetype":
        return "Primary"
    if key == "imageindex":
        return 0
    if key == "index" or key == "routeindex":
        return 0
    if key == "width":
        return 320
    if key in {"maxwidth", "maxheight"}:
        return 320
    if key == "percentplayed" or key == "unplayedcount":
        return 0
    if key == "format" or key == "routeformat":
        return "vtt"
    if key == "container":
        return fixtures.container or "mkv"
    if key == "tag":
        return "openapi"
    if key == "language":
        return "eng"
    if key == "newindex":
        return 0
    if key == "year":
        return 2024
    if key == "command":
        return "DisplayMessage"
    if key == "pluginid":
        return "openapi-compat"
    if key == "version":
        return "0.0.0"
    if key == "taskid":
        return "scan-library"
    if key in {"timerid", "tunerid", "programid", "recordingid", "streamid", "subtitleid", "lyricid", "packageid", "id"}:
        return FAKE_UUID_2
    if key == "routestartpositionticks":
        return 0
    return "openapi"


def name_value_for_path(path: str, fixtures: Fixtures) -> str:
    if path.startswith("/Artists/"):
        return fixtures.artist_name
    if path.startswith("/Genres/"):
        return fixtures.genre_name
    if path.startswith("/MusicGenres/"):
        return fixtures.music_genre_name
    if path.startswith("/Persons/"):
        return fixtures.person_name
    if path.startswith("/Studios/"):
        return fixtures.studio_name
    if path.startswith("/FallbackFont/Fonts/"):
        return "Arial"
    if path.startswith("/Images/"):
        return "openapi"
    return "openapi"


def query_params(operation: Operation, fixtures: Fixtures) -> dict[str, str]:
    values: dict[str, str] = {}
    for parameter in operation.parameters:
        if parameter.get("in") != "query":
            continue
        name = parameter.get("name")
        if not name:
            continue
        if parameter.get("required") or name in OPTIONAL_QUERY_VALUES:
            values[name] = str(query_value(name, parameter.get("schema") or {}, operation, fixtures))
    return values


def query_value(name: str, schema: dict[str, Any], operation: Operation, fixtures: Fixtures) -> str | int | bool:
    if "enum" in schema and schema["enum"]:
        return schema["enum"][0]
    key = name.lower()
    if key == "userid":
        return fixtures.user_id
    if key in {"itemid", "ids", "itemids"}:
        return fixtures.item_id
    if key == "app":
        return "OpenAPI Compat"
    if key == "client":
        return CLIENT
    if key == "id":
        return DEVICE_ID
    if key == "path":
        return "/tmp"
    if key == "name":
        return fixtures.log_name if operation.path.startswith("/System/Logs") else "openapi"
    if key == "searchterm":
        return "a"
    if key == "livestreamid":
        return FAKE_UUID
    if key == "playcommand":
        return "PlayNow"
    if key == "playsessionid":
        return fixtures.play_session_id
    if key == "itemname":
        return "OpenAPI Item"
    if key == "itemtype":
        return "Movie"
    if key == "type":
        return "Primary"
    if key == "newindex":
        return 0
    if key == "segmentlength":
        return 30
    if key == "filename":
        return "openapi.lrc"
    if key == "limit":
        return 1
    if key == "providername" and operation.operation_id == "GetRemoteImages":
        return "Local"
    if key == "recursive":
        return True
    if key == "fields":
        return "MediaSources,Overview"
    schema_type = schema.get("type")
    if schema_type == "boolean":
        return True
    if schema_type == "integer":
        return 1
    return "openapi"


def body_for_operation(operation: Operation, fixtures: Fixtures, username: str, password: str) -> tuple[Any, str | None]:
    if operation.operation_id == "AuthenticateUserByName":
        return {"Username": username, "Pw": password}, "application/json"
    if operation.operation_id == "GetPostedPlaybackInfo":
        return {}, "application/json"
    if operation.operation_id in {"ReportPlaybackStart", "ReportPlaybackProgress", "ReportPlaybackStopped"}:
        return playback_body(fixtures), "application/json"
    if operation.operation_id in {"PostCapabilities", "PostFullCapabilities"}:
        return {"PlayableMediaTypes": ["Video"], "SupportedCommands": []}, "application/json"
    if operation.operation_id == "ReportViewing":
        return {"ItemId": fixtures.item_id, "SessionId": fixtures.session_id}, "application/json"
    if not operation.request_body:
        return None, None
    content = operation.request_body.get("content") or {}
    if "application/json" in content:
        schema = content["application/json"].get("schema") or {}
        return sample_from_schema(schema), "application/json"
    if "text/plain" in content:
        return "openapi compatibility probe", "text/plain"
    if content:
        return b"openapi", next(iter(content))
    return None, None


def sample_from_schema(schema: dict[str, Any], seen: set[str] | None = None) -> Any:
    seen = seen or set()
    if "$ref" in schema:
        return {}
    if "enum" in schema and schema["enum"]:
        return schema["enum"][0]
    if "allOf" in schema and schema["allOf"]:
        return sample_from_schema(schema["allOf"][0], seen)
    schema_type = schema.get("type")
    if schema_type == "array":
        return []
    if schema_type == "integer":
        return 1
    if schema_type == "number":
        return 1.0
    if schema_type == "boolean":
        return False
    if schema_type == "string":
        if schema.get("format") == "uuid":
            return FAKE_UUID
        return "openapi"
    properties = schema.get("properties") or {}
    required = schema.get("required") or []
    if properties:
        return {name: sample_from_schema(properties[name], seen) for name in required if name in properties}
    return {}


def playback_body(fixtures: Fixtures) -> dict[str, Any]:
    return {
        "VolumeLevel": 100,
        "NowPlayingQueue": [],
        "IsMuted": False,
        "IsPaused": False,
        "MaxStreamingBitrate": 2147483647,
        "RepeatMode": "RepeatNone",
        "PlaybackStartTimeTicks": 0,
        "SubtitleOffset": 0,
        "PlaybackRate": 1,
        "PositionTicks": 0,
        "PlayMethod": "DirectPlay",
        "PlaySessionId": fixtures.play_session_id,
        "LiveStreamId": "",
        "MediaSourceId": fixtures.media_source_id,
        "PlaylistIndex": 0,
        "PlaylistLength": 1,
        "CanSeek": True,
        "ItemId": fixtures.item_id,
        "Shuffle": False,
    }


def probe_operation(client: JellyfinClient, operation: Operation) -> ProbeResult:
    fixtures = client.fixtures
    concrete_path = render_path(operation.path, operation, fixtures)
    params = query_params(operation, fixtures)
    auth_probe = operation.is_mutating and operation.operation_id not in SAFE_AUTH_MUTATIONS
    authenticated = not auth_probe
    mode = "auth" if authenticated else "auth-gate"
    body, content_type = body_for_operation(operation, fixtures, client.username, client.password)
    headers = client.headers(authenticated=authenticated, content_type=content_type)
    if operation.method == "GET" and any(part in operation.path for part in ["/Videos/", "/Audio/", "/LiveTv/LiveStreamFiles/"]):
        headers["Range"] = "bytes=0-0"
    try:
        request = client.http.build_request(
            operation.method,
            client.url(concrete_path),
            headers=headers,
            params=params,
            json=body if content_type == "application/json" else None,
            content=body if content_type and content_type != "application/json" else None,
        )
        response = client.http.send(request, stream=True)
        body_sample = read_body_sample(response, operation.method)
    except httpx.HTTPError as error:
        return ProbeResult(operation, None, False, mode, f"http error: {error}")

    ok, detail = classify_response(operation, response.status_code, body_sample, auth_probe)
    return ProbeResult(operation, response.status_code, ok, mode, detail)


def read_body_sample(response: httpx.Response, method: str) -> str:
    if method == "HEAD":
        response.close()
        return ""
    chunks: list[bytes] = []
    total = 0
    try:
        for chunk in response.iter_bytes():
            chunks.append(chunk)
            total += len(chunk)
            if total >= 4096:
                break
    finally:
        response.close()
    return b"".join(chunks)[:4096].decode("utf-8", errors="replace")


def classify_response(operation: Operation, status: int, body_text: str, auth_probe: bool) -> tuple[bool, str]:
    body = body_text[:240].replace("\n", " ") if status >= 400 and operation.method != "HEAD" else ""
    if status == 405:
        return False, "method not allowed"
    if status >= 500:
        return False, f"server error: {body}"
    if auth_probe:
        if status in {401, 403}:
            return True, "protected"
        public_mutating = operation.path in {
            "/Users/AuthenticateByName",
            "/Users/ForgotPassword",
            "/Users/ForgotPassword/Pin",
        }
        if public_mutating and status < 500:
            return True, "public mutating probe"
        return False, f"mutating operation was not protected: {status} {body}"
    if status == 401:
        return False, f"authenticated request rejected: {body}"
    if status == 403:
        return False, f"authenticated admin request forbidden: {body}"
    return True, ""


def print_summary(results: list[ProbeResult], skipped: list[Operation], verbose: bool) -> None:
    status_counts = Counter(result.status for result in results)
    mode_counts = Counter(result.mode for result in results)
    failures = [result for result in results if not result.ok]
    print(f"Runtime probes: {len(results)} checked, {len(skipped)} quick-connect skipped, {len(failures)} failed")
    print("Probe modes:", ", ".join(f"{key}={value}" for key, value in sorted(mode_counts.items())))
    print("Statuses:", ", ".join(f"{key}={value}" for key, value in sorted(status_counts.items(), key=lambda kv: str(kv[0]))))
    if verbose:
        for result in results:
            status = result.status if result.status is not None else "ERR"
            marker = "PASS" if result.ok else "FAIL"
            print(f"[{marker}] {status} {result.mode} {result.operation.label}")
    if failures:
        print("\nFailures:")
        for result in failures[:80]:
            status = result.status if result.status is not None else "ERR"
            print(f"- {status} {result.operation.label}: {result.detail}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Probe jellyfin-rs against the Jellyfin OpenAPI spec.")
    parser.add_argument("--base-url", default="http://127.0.0.1:8096")
    parser.add_argument("--username", default="admin")
    parser.add_argument("--password", default="123456")
    parser.add_argument("--spec", default="docs/jellyfin-openapi-stable.json")
    parser.add_argument("--routes-source", default="src/jellyfin/routes.rs")
    parser.add_argument("--timeout", type=float, default=8.0)
    parser.add_argument("--progress-every", type=int, default=25)
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    spec_path = Path(args.spec)
    routes_source = Path(args.routes_source)
    operations = load_operations(spec_path)
    checked_operations = [operation for operation in operations if not operation.is_quick_connect]
    skipped = [operation for operation in operations if operation.is_quick_connect]

    client = JellyfinClient(args.base_url, args.username, args.password, args.timeout)
    client.login()
    client.discover_fixtures()

    routes = extract_axum_routes(routes_source)
    missing_routes = static_route_check(checked_operations, routes, client.fixtures)
    print(
        f"Static route coverage: {len(checked_operations)} OpenAPI operations checked, "
        f"{len(missing_routes)} missing"
    )
    if missing_routes:
        for label in missing_routes[:80]:
            print(f"- missing route: {label}")
        return 1

    results: list[ProbeResult] = []
    for index, operation in enumerate(checked_operations, start=1):
        results.append(probe_operation(client, operation))
        if args.verbose or (args.progress_every > 0 and index % args.progress_every == 0):
            print(f"Probed {index}/{len(checked_operations)} operations", flush=True)
    print_summary(results, skipped, args.verbose)
    return 1 if any(not result.ok for result in results) else 0


if __name__ == "__main__":
    sys.exit(main())
