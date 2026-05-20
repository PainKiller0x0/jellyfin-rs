#!/bin/bash
# Test API using tsukimi client patterns:
# - /emby/ base path prefix
# - Emby auth header with UserId
# - X-Emby-Token header for authenticated requests
# - Full session bodies matching position_back()
BASE="http://localhost:8096/emby"
PASS=0; FAIL=0; WARN=0
CLIENT="tsukimi"
DEVICE="test-device"
DEVICE_ID="test-device-id"
VERSION="0.1.0"

# Step 1: Authenticate (matching tsukimi's login)
echo "=== Authenticating ==="
LOGIN_RESP=$(curl -sS -X POST "$BASE/Users/authenticatebyname" \
  -H "Content-Type: application/json" \
  -H "X-Emby-Authorization: Emby Client=\"$CLIENT\",Device=\"$DEVICE\",DeviceId=\"$DEVICE_ID\",Version=\"$VERSION\"" \
  -d '{"Username":"tsukimi","Pw":"tsukimi"}')
ACCESS_TOKEN=$(echo "$LOGIN_RESP" | grep -o '"AccessToken":"[^"]*"' | cut -d'"' -f4)
USER_ID=$(echo "$LOGIN_RESP" | grep -o '"Id":"[^"]*"' | head -1 | cut -d'"' -f4)
echo "  Token: $ACCESS_TOKEN"
echo "  UserId: $USER_ID"

if [ -z "$ACCESS_TOKEN" ] || [ -z "$USER_ID" ]; then
  echo "  [FAIL] Authentication failed"
  echo "  Response: $LOGIN_RESP"
  exit 1
fi
echo "  [PASS] Authenticated"

# Auth header with UserId (as tsukimi does after init)
AUTH="Emby UserId=$USER_ID,Client=$CLIENT,Device=$DEVICE,DeviceId=$DEVICE_ID,Version=$VERSION"

t() {
  local method="$1" url="$2" data="$3" desc="$4"
  local code
  if [ "$method" = "POST" ]; then
    code=$(curl -sS -o /tmp/t.json -w "%{http_code}" -X POST "$BASE$url" \
      -H "Content-Type: application/json" \
      -H "X-Emby-Token: $ACCESS_TOKEN" \
      -H "X-Emby-Authorization: $AUTH" \
      -d "$data" 2>/dev/null)
  elif [ "$method" = "DEL" ]; then
    code=$(curl -sS -o /tmp/t.json -w "%{http_code}" -X DELETE "$BASE$url" \
      -H "Content-Type: application/json" \
      -H "X-Emby-Token: $ACCESS_TOKEN" \
      -H "X-Emby-Authorization: $AUTH" \
      -d "$data" 2>/dev/null)
  else
    code=$(curl -sS -o /tmp/t.json -w "%{http_code}" "$BASE$url" \
      -H "X-Emby-Token: $ACCESS_TOKEN" \
      -H "X-Emby-Authorization: $AUTH" 2>/dev/null)
  fi
  local brief=$(head -c 100 /tmp/t.json 2>/dev/null)
  case $code in
    200) echo "  [PASS] $code  $desc"; ((PASS++)) ;;
    204) echo "  [PASS] $code  $desc"; ((PASS++)) ;;
    400) echo "  [WARN] $code  $desc — $brief"; ((WARN++)) ;;
    404) echo "  [WARN] $code  $desc — $brief"; ((WARN++)) ;;
    405) echo "  [WARN] $code  $desc (not implemented)"; ((WARN++)) ;;
    *)   echo "  [FAIL] $code  $desc — $brief"; ((FAIL++)) ;;
  esac
}

echo ""
echo "============================================"
echo "  SYSTEM (tsukimi: get_server_info, etc.)"
echo "============================================"
t GET "/System/Info"              "" "System Info"
t GET "/System/Info/Public"       "" "Public System Info"
t GET "/System/Ping"              "" "Ping"
t GET "/System/ActivityLog/Entries?hasUserId=false" "" "Activity Log"
t GET "/ScheduledTasks"           "" "Scheduled Tasks"

echo ""
echo "============================================"
echo "  LIBRARY (tsukimi: get_library → Users/{id}/Views)"
echo "============================================"
t GET "/Users/$USER_ID/Views"     "" "User Views (library)"

echo ""
echo "============================================"
echo "  ITEMS (tsukimi: browse, search, detail)"
echo "============================================"
# Search (tsukimi pattern)
t GET "/Users/$USER_ID/Items?SearchTerm=Matrix&IncludeItemTypes=Movie&Recursive=true&Limit=10&SortBy=SortName&SortOrder=Ascending&Fields=Overview,PrimaryImageAspectRatio,ProductionYear" "" "Search: Matrix"
# Item detail
t GET "/Users/$USER_ID/Items/movie-1?Fields=ShareLevel" "" "Item: The Matrix"
# Latest
t GET "/Users/$USER_ID/Items/Latest?ParentId=movies&Limit=10&Fields=Overview,PrimaryImageAspectRatio,ProductionYear&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb,Banner" "" "Latest items"
# Resume
t GET "/Users/$USER_ID/Items/Resume?Recursive=true&MediaTypes=Video&Fields=Overview,PrimaryImageAspectRatio,ProductionYear&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb,Banner" "" "Resume items"
# Similar
t GET "/Items/movie-1/Similar?UserId=$USER_ID&Limit=10&ImageTypeLimit=1" "" "Similar items"
# External IDs
t GET "/Items/movie-1/ExternalIdInfos?IsSupportedAsIdentifier=true" "" "External IDs"
# Item images
t GET "/Items/movie-1/Images"    "" "Item images"

echo ""
echo "============================================"
echo "  SHOWS (tsukimi: episodes, seasons, next up)"
echo "============================================"
t GET "/Shows/tv-bb/Episodes?UserId=$USER_ID&SeasonId=tv-bb-s1&Fields=Overview,PrimaryImageAspectRatio,PremiereDate,ProductionYear" "" "Breaking Bad S1 episodes"
t GET "/Shows/tv-bb/Seasons?UserId=$USER_ID&ImageTypeLimit=1&Fields=Overview,PremiereDate" "" "Breaking Bad seasons"
t GET "/Shows/NextUp?UserId=$USER_ID&Fields=Overview&Limit=10&ImageTypeLimit=1" "" "Next Up"
t GET "/Shows/Missing?UserId=$USER_ID&ParentId=tv-bb&IncludeSpecials=false&IncludeUnaired=false" "" "Missing episodes"

echo ""
echo "============================================"
echo "  PLAYBACK (tsukimi: position_back, exact body)"
echo "============================================"
# Session start - full body matching tsukimi position_back(BackType::Start)
START_BODY='{"VolumeLevel":100,"NowPlayingQueue":[],"IsMuted":false,"IsPaused":false,"MaxStreamingBitrate":2147483647,"RepeatMode":"RepeatNone","PlaybackStartTimeTicks":0,"SubtitleOffset":0,"PlaybackRate":1,"PositionTicks":0,"PlayMethod":"DirectPlay","PlaySessionId":"test-session-1","LiveStreamId":"","MediaSourceId":"","PlaylistIndex":0,"PlaylistLength":1,"CanSeek":true,"ItemId":"movie-1","Shuffle":false}'
t POST "/Sessions/Playing"       "$START_BODY" "Start playback (full body)"

# Session progress
PROGRESS_BODY='{"VolumeLevel":100,"NowPlayingQueue":[],"IsMuted":false,"IsPaused":false,"MaxStreamingBitrate":2147483647,"RepeatMode":"RepeatNone","PlaybackStartTimeTicks":0,"SubtitleOffset":0,"PlaybackRate":1,"PositionTicks":100000000,"PlayMethod":"DirectPlay","PlaySessionId":"test-session-1","LiveStreamId":"","MediaSourceId":"","PlaylistIndex":0,"PlaylistLength":1,"CanSeek":true,"ItemId":"movie-1","Shuffle":false}'
t POST "/Sessions/Playing/Progress" "$PROGRESS_BODY" "Progress (full body)"

# Session stop
STOP_BODY='{"VolumeLevel":100,"NowPlayingQueue":[],"IsMuted":false,"IsPaused":false,"MaxStreamingBitrate":2147483647,"RepeatMode":"RepeatNone","PlaybackStartTimeTicks":0,"SubtitleOffset":0,"PlaybackRate":1,"PositionTicks":500000000,"PlayMethod":"DirectPlay","PlaySessionId":"test-session-1","LiveStreamId":"","MediaSourceId":"","PlaylistIndex":0,"PlaylistLength":1,"CanSeek":true,"ItemId":"movie-1","Shuffle":false}'
t POST "/Sessions/Playing/Stopped" "$STOP_BODY" "Stop (full body)"

echo ""
echo "============================================"
echo "  USER ACTIONS (tsukimi: like, played, hide)"
echo "============================================"
# Favorite (tsukimi: like)
t POST "/Users/$USER_ID/FavoriteItems/movie-2" "{}" "Favorite movie-2"
t POST "/Users/$USER_ID/FavoriteItems/movie-2/Delete" "{}" "Unfavorite movie-2"

# Mark played (tsukimi: set_as_played)
t POST "/Users/$USER_ID/PlayedItems/movie-2" "{}" "Mark played"
t POST "/Users/$USER_ID/PlayedItems/movie-2/Delete" "{}" "Mark unplayed"

# Hide from resume (tsukimi: hide_from_resume)
t POST "/Users/$USER_ID/Items/movie-2/HideFromResume?Hide=true" "{}" "Hide from resume"

# User avatar
t GET "/Users/$USER_ID/Images/Primary?maxHeight=50&maxWidth=50" "" "User avatar"

echo ""
echo "============================================"
echo "  PASSWORD (tsukimi: change_password)"
echo "============================================"
t POST "/Users/$USER_ID/Password" '{"CurrentPw":"tsukimi","NewPw":"tsukimi"}' "Change password"

echo ""
echo "============================================"
echo "  FILTERS (tsukimi: filters → Genres/Studios/etc.)"
echo "============================================"
t GET "/Genres?SortBy=SortName&SortOrder=Ascending&Recursive=true&EnableImages=false&EnableUserData=false&IncludeItemTypes=Movie,Series,Episode,BoxSet,Person,MusicAlbum,Audio,Video&userId=$USER_ID" "" "Genres (tsukimi params)"
t GET "/Studios?SortBy=SortName&SortOrder=Ascending&Recursive=true&EnableImages=false&EnableUserData=false&userId=$USER_ID" "" "Studios"
t GET "/Persons?SortBy=SortName&SortOrder=Ascending&Recursive=true&EnableImages=false&EnableUserData=false&userId=$USER_ID" "" "Persons"
t GET "/Tags?SortBy=SortName&SortOrder=Ascending&Recursive=true&EnableImages=false&EnableUserData=false&userId=$USER_ID" "" "Tags"

echo ""
echo "============================================"
echo "  PERSONS (tsukimi: get_actor_item_list)"
echo "============================================"
t GET "/Users/$USER_ID/Items?PersonIds=christopher-nolan&Recursive=true&CollapseBoxSetItems=false&SortBy=SortName&SortOrder=Ascending&IncludeItemTypes=Movie&ImageTypeLimit=1&Limit=12" "" "Nolan movies"
t GET "/Users/$USER_ID/Items?PersonIds=keanu-reeves&Recursive=true&CollapseBoxSetItems=false&SortBy=SortName&SortOrder=Ascending&IncludeItemTypes=Movie&ImageTypeLimit=1&Limit=12" "" "Keanu Reeves movies"

echo ""
echo "============================================"
echo "  DELETE / SCAN (tsukimi patterns)"
echo "============================================"
t POST "/Items/Delete?Ids=movie-1" "{}" "Delete movie-1"
t POST "/Items/movie-2/Refresh?Recursive=true&ImageRefreshMode=Default&MetadataRefreshMode=Default&ReplaceAllImages=false&ReplaceAllMetadata=false" "{}" "Refresh movie-2"

echo ""
echo "============================================"
echo "  COLLECTIONS"
echo "============================================"
t POST "/Collections?name=TestCollection" "" "Create collection"
CID=$(curl -sS "$BASE/Collections?api_key=$ACCESS_TOKEN&name=TestCollection" 2>/dev/null | grep -o '"Id":"[^"]*"' | cut -d'"' -f4)
t POST "/Collections/$CID/Items" '{"Ids":["movie-2","movie-3"]}' "Add items to collection"
t DEL "/Collections/$CID/Items"  '{"Ids":["movie-3"]}' "Remove item from collection"

echo ""
echo "============================================"
echo "  SUMMARY: $PASS passed, $WARN warnings, $FAIL failed"
echo "============================================"
