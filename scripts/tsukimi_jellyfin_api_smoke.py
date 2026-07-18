#!/usr/bin/env python3
import argparse
import json
import sys
from dataclasses import dataclass
from typing import Any
from urllib.parse import urljoin

import httpx


CLIENT = "Codex Jellyfin API"
DEVICE = "Docker API Smoke"
DEVICE_ID = "codex-jellyfin-api"
VERSION = "0.0.0"


@dataclass
class CheckResult:
    name: str
    ok: bool
    detail: str = ""


class JellyfinApi:
    def __init__(self, base_url: str, username: str, password: str) -> None:
        self.base_url = base_url.rstrip("/") + "/"
        self.username = username
        self.password = password
        self.client = httpx.Client(timeout=30.0, follow_redirects=False)
        self.token = ""
        self.user_id = ""
        self.headers = {
            "Accept": "application/json",
            "Authorization": self.auth_header(),
        }

    def auth_header(self, include_token: bool = False) -> str:
        parts = [
            f'Client="{CLIENT}"',
            f'Device="{DEVICE}"',
            f'DeviceId="{DEVICE_ID}"',
            f'Version="{VERSION}"',
        ]
        if self.user_id:
            parts.insert(0, f'UserId="{self.user_id}"')
        if include_token and self.token:
            parts.append(f'Token="{self.token}"')
        return "MediaBrowser " + ", ".join(parts)

    def url(self, path: str) -> str:
        return urljoin(self.base_url, path.lstrip("/"))

    def login(self) -> dict[str, Any]:
        response = self.client.post(
            self.url("Users/AuthenticateByName"),
            headers=self.headers | {"Content-Type": "application/json"},
            json={"Username": self.username, "Pw": self.password},
        )
        response.raise_for_status()
        data = response.json()
        self.token = data["AccessToken"]
        self.user_id = data["User"]["Id"]
        self.headers = {
            "Accept": "application/json",
            "Authorization": self.auth_header(include_token=True),
        }
        return data

    def get(self, path: str, **params: Any) -> httpx.Response:
        return self.client.get(self.url(path), headers=self.headers, params=clean_params(params))

    def post(self, path: str, body: Any | None = None, **params: Any) -> httpx.Response:
        return self.client.post(
            self.url(path),
            headers=self.headers | {"Content-Type": "application/json"},
            params=clean_params(params),
            json={} if body is None else body,
        )

    def delete(self, path: str, **params: Any) -> httpx.Response:
        return self.client.delete(self.url(path), headers=self.headers, params=clean_params(params))

    def stream_url(self, direct_url: str) -> str:
        if direct_url.startswith("http://") or direct_url.startswith("https://"):
            return direct_url
        if has_auth_query(direct_url):
            return self.url(direct_url)
        separator = "&" if "?" in direct_url else "?"
        return self.url(direct_url) + f"{separator}api_key={self.token}"


def clean_params(values: dict[str, Any]) -> dict[str, str]:
    return {
        key: str(value)
        for key, value in values.items()
        if value is not None and value != ""
    }


def has_auth_query(url: str) -> bool:
    query = url.split("?", 1)[1] if "?" in url else ""
    keys = {part.split("=", 1)[0].lower() for part in query.split("&") if part}
    return bool(keys & {"api_key", "apikey", "token", "access_token", "accesstoken"})


def expect_status(name: str, response: httpx.Response, *statuses: int) -> CheckResult:
    if response.status_code in statuses:
        return CheckResult(name, True, str(response.status_code))
    body = response.text[:240].replace("\n", " ")
    return CheckResult(name, False, f"{response.status_code}: {body}")


def expect_json(name: str, response: httpx.Response, *statuses: int) -> tuple[CheckResult, Any]:
    result = expect_status(name, response, *statuses)
    if not result.ok:
        return result, None
    try:
        return result, response.json()
    except json.JSONDecodeError as error:
        return CheckResult(name, False, f"invalid json: {error}"), None


def first_item(items: list[dict[str, Any]], *types: str) -> dict[str, Any] | None:
    wanted = {item_type.lower() for item_type in types}
    return next((item for item in items if item.get("Type", "").lower() in wanted), None)


def playback_body(item_id: str, play_session_id: str, position_ticks: int) -> dict[str, Any]:
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
        "PositionTicks": position_ticks,
        "PlayMethod": "DirectPlay",
        "PlaySessionId": play_session_id,
        "LiveStreamId": "",
        "MediaSourceId": "",
        "PlaylistIndex": 0,
        "PlaylistLength": 1,
        "CanSeek": True,
        "ItemId": item_id,
        "Shuffle": False,
    }


def run(args: argparse.Namespace) -> int:
    api = JellyfinApi(args.base_url, args.username, args.password)
    checks: list[CheckResult] = []

    public = api.client.get(api.url("System/Info/Public"), timeout=10.0)
    checks.append(expect_status("public system info", public, 200))

    login = api.login()
    session_info = login.get("SessionInfo", {})
    checks.append(
        CheckResult(
            "login returns Jellyfin session client",
            session_info.get("Client") == CLIENT,
            f"Client={session_info.get('Client')!r}",
        )
    )

    emby_prefixed = api.client.get(api.url("emby/System/Info/Public"), timeout=10.0)
    checks.append(expect_status("emby prefix is not exposed", emby_prefixed, 404))

    emby_token = api.client.get(
        api.url("System/Info"),
        headers={"Accept": "application/json", "X-Emby-Token": api.token},
        timeout=10.0,
    )
    checks.append(expect_status("X-Emby-Token is not accepted", emby_token, 401))

    for name, response in [
        ("system info", api.get("System/Info")),
        ("ping", api.get("System/Ping")),
        ("scheduled tasks", api.get("ScheduledTasks")),
        ("activity log", api.get("System/ActivityLog/Entries", hasUserId="false")),
        ("users me", api.get("Users/Me")),
        ("sessions", api.get("Sessions")),
        ("library media folders", api.get("Library/MediaFolders")),
        ("library views", api.get(f"Users/{api.user_id}/Views")),
        ("resume items", api.get(f"Users/{api.user_id}/Items/Resume", Recursive="true", MediaTypes="Video")),
        ("latest items", api.get(f"Users/{api.user_id}/Items/Latest", Limit=20)),
        ("filters genres", api.get("Genres", userId=api.user_id)),
        ("filters persons", api.get("Persons", userId=api.user_id)),
        ("filters studios", api.get("Studios", userId=api.user_id)),
        ("filters tags", api.get("Tags", userId=api.user_id)),
    ]:
        checks.append(expect_status(name, response, 200))

    item_check, item_list = expect_json(
        "items list",
        api.get(
            f"Users/{api.user_id}/Items",
            Recursive="true",
            IncludeItemTypes="Movie,Episode,Video,Series,Season",
            Fields="Overview,PrimaryImageAspectRatio,ProductionYear,MediaSources,Path",
            SortBy="SortName",
            SortOrder="Ascending",
            Limit=200,
        ),
        200,
    )
    checks.append(item_check)
    items = item_list.get("Items", []) if isinstance(item_list, dict) else []
    playable = first_item(items, "Movie", "Episode", "Video")
    series = first_item(items, "Series")
    season = first_item(items, "Season")
    checks.append(CheckResult("found playable video item", playable is not None))

    if series:
        checks.append(expect_status("show seasons", api.get(f"Shows/{series['Id']}/Seasons", UserId=api.user_id), 200))
        checks.append(expect_status("show episodes", api.get(f"Shows/{series['Id']}/Episodes", UserId=api.user_id), 200))
    if season:
        checks.append(
            expect_status(
                "season children",
                api.get(f"Users/{api.user_id}/Items", ParentId=season["Id"], Fields="MediaSources"),
                200,
            )
        )

    if playable:
        item_id = playable["Id"]
        detail_check, detail = expect_json(
            "item detail",
            api.get(f"Users/{api.user_id}/Items/{item_id}", Fields="ShareLevel,MediaSources,Path"),
            200,
        )
        checks.append(detail_check)
        checks.append(expect_status("item images", api.get(f"Items/{item_id}/Images"), 200))
        checks.append(expect_status("similar items", api.get(f"Items/{item_id}/Similar", UserId=api.user_id, Limit=10), 200))
        checks.append(expect_status("external id infos", api.get(f"Items/{item_id}/ExternalIdInfos", IsSupportedAsIdentifier="true"), 200))

        playback_check, playback = expect_json(
            "playback info",
            api.post(
                f"Items/{item_id}/PlaybackInfo",
                body={},
                UserId=api.user_id,
                IsPlayback="true",
                AutoOpenLiveStream="true",
                MaxStreamingBitrate=2147483647,
                EnableDirectPlay="true",
                EnableDirectStream="true",
            ),
            200,
        )
        checks.append(playback_check)

        sources = playback.get("MediaSources", []) if isinstance(playback, dict) else []
        source = sources[0] if sources else None
        direct_url = source.get("DirectStreamUrl") if isinstance(source, dict) else None
        streams = source.get("MediaStreams", []) if isinstance(source, dict) else []
        checks.append(CheckResult("playback has media source", source is not None))
        checks.append(CheckResult("playback has media streams", bool(streams), f"count={len(streams)}"))
        checks.append(CheckResult("playback has direct stream url", bool(direct_url), str(direct_url)))

        if direct_url:
            stream_url = api.stream_url(direct_url)
            head = api.client.head(stream_url, headers={"Range": "bytes=0-0"}, timeout=30.0)
            checks.append(expect_status("stream HEAD", head, 200, 206))
            get = api.client.get(stream_url, headers={"Range": "bytes=0-1023"}, timeout=30.0)
            checks.append(expect_status("stream range GET", get, 206))
            checks.append(CheckResult("stream returned bytes", len(get.content) > 0, f"bytes={len(get.content)}"))

        play_session_id = playback.get("PlaySessionId") if isinstance(playback, dict) else "codex-playback"
        play_session_id = play_session_id or "codex-playback"
        checks.append(expect_status("playback start", api.post("Sessions/Playing", playback_body(item_id, play_session_id, 0)), 204))
        checks.append(
            expect_status(
                "playback progress",
                api.post("Sessions/Playing/Progress", playback_body(item_id, play_session_id, 10_000_000)),
                204,
            )
        )
        checks.append(
            expect_status(
                "playback stopped",
                api.post("Sessions/Playing/Stopped", playback_body(item_id, play_session_id, 20_000_000)),
                204,
            )
        )
        checks.append(expect_status("favorite", api.post(f"Users/{api.user_id}/FavoriteItems/{item_id}"), 200, 204))
        checks.append(expect_status("unfavorite", api.delete(f"Users/{api.user_id}/FavoriteItems/{item_id}"), 200, 204))
        checks.append(expect_status("mark played", api.post(f"Users/{api.user_id}/PlayedItems/{item_id}"), 200, 204))
        checks.append(expect_status("mark unplayed", api.delete(f"Users/{api.user_id}/PlayedItems/{item_id}"), 200, 204))
        checks.append(expect_status("hide from resume", api.post(f"Users/{api.user_id}/Items/{item_id}/HideFromResume", Hide="true"), 200, 204))

    failed = [check for check in checks if not check.ok]
    for check in checks:
        marker = "PASS" if check.ok else "FAIL"
        suffix = f" - {check.detail}" if check.detail else ""
        print(f"[{marker}] {check.name}{suffix}")

    print(f"\nSummary: {len(checks) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8096")
    parser.add_argument("--username", default="admin")
    parser.add_argument("--password", default="123456")
    args = parser.parse_args()
    try:
        return run(args)
    except Exception as error:
        print(f"fatal: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
