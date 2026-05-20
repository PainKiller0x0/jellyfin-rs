#!/bin/bash
BASE="http://localhost:8096"
PASS=0; FAIL=0; WARN=0

# get token
TOKEN=$(curl -sS -X POST "$BASE/Users/AuthenticateByName" \
  -H "Content-Type: application/json" \
  -H "X-Emby-Authorization: MediaBrowser Client=\"test\", Device=\"test\", DeviceId=\"test\", Version=\"1.0\"" \
  -d '{"Username":"tsukimi","Pw":"tsukimi"}' | grep -o '"AccessToken":"[^"]*"' | cut -d'"' -f4)

t() {
  local method="$1" url="$2" data="$3" desc="$4"
  local code
  if [ "$method" = "POST" ]; then
    code=$(curl -sS -o /tmp/t.json -w "%{http_code}" -X POST "$BASE${url}?api_key=$TOKEN" \
      -H "Content-Type: application/json" -d "$data" 2>/dev/null)
  elif [ "$method" = "DEL" ]; then
    code=$(curl -sS -o /tmp/t.json -w "%{http_code}" -X DELETE "$BASE${url}?api_key=$TOKEN" \
      -H "Content-Type: application/json" -d "$data" 2>/dev/null)
  else
    code=$(curl -sS -o /tmp/t.json -w "%{http_code}" "$BASE${url}?api_key=$TOKEN" 2>/dev/null)
  fi
  local brief=$(head -c 90 /tmp/t.json 2>/dev/null)
  case $code in
    200) echo "  [PASS] $code  $desc"; ((PASS++)) ;;
    204) echo "  [PASS] $code  $desc"; ((PASS++)) ;;
    400) echo "  [WARN] $code  $desc — $brief"; ((WARN++)) ;;
    404) echo "  [WARN] $code  $desc — $brief"; ((WARN++)) ;;
    405) echo "  [WARN] $code  $desc (not implemented)"; ((WARN++)) ;;
    *)   echo "  [FAIL] $code  $desc — $brief"; ((FAIL++)) ;;
  esac
}

echo "============================================"
echo "  SYSTEM"
echo "============================================"
t GET "/System/Ping"              "" "Ping"
t GET "/GetUtcTime"               "" "GetUtcTime"
t GET "/System/Info"              "" "System Info"
t GET "/System/Info/Public"       "" "Public Info"
t GET "/System/Endpoint"          "" "WanEndpoint"
t GET "/System/Configuration"     "" "Server Config"
t GET "/System/Logs"              "" "Logs list"
t GET "/System/Info/Storage"      "" "Storage info"
t GET "/ScheduledTasks"           "" "ScheduledTasks"
t GET "/System/ActivityLog/Entries" "" "ActivityLog"
t GET "/Devices"                  "" "Devices"
t GET "/Devices/Info"             "" "Device Info"
t GET "/Devices/Options"          "" "Device Options"
t GET "/web/ConfigurationPages"   "" "ConfigPages"
t GET "/Startup/Configuration"    "" "Startup Config"
t GET "/Startup/User"             "" "Startup User"
t GET "/Branding/Configuration"   "" "Branding"
t GET "/QuickConnect/Enabled"     "" "QuickConnect"
t GET "/Tmdb/ClientConfiguration" "" "Tmdb Config"
t GET "/Playback/BitrateTest"     "" "BitrateTest"

echo ""
echo "============================================"
echo "  LOCALIZATION"
echo "============================================"
t GET "/Cultures"                 "" "Cultures"
t GET "/Countries"                "" "Countries"
t GET "/ParentalRatings"          "" "ParentalRatings"
t GET "/Localization/Options"     "" "Localization Options"

echo ""
echo "============================================"
echo "  AUTH / USERS"
echo "============================================"
t GET "/Users"                    "" "List Users"
t GET "/Users/Public"             "" "Public Users"
t GET "/Users/Me"                 "" "Current User"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328" "" "Get User by ID"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Views" "" "User Views"
t POST "/Users/authenticatebyname" '{"Username":"tsukimi","Pw":"tsukimi"}' "Auth lower"
t POST "/Users/ForgotPassword"    '{"EnteredUsername":"tsukimi"}' "ForgotPassword"
t POST "/Users/New"               '{"Name":"api-test-user","Password":"p4ss"}' "Create User"

echo ""
echo "============================================"
echo "  API KEYS"
echo "============================================"
t GET "/Auth/Keys"                "" "List Keys"
# Find and delete the new user
NEWUID=$(curl -sS "$BASE/Users?api_key=$TOKEN" | grep -o '"Id":"[^"]*"' | grep -v "57b1e08d" | head -1 | cut -d'"' -f4)
t DEL "/Users/$NEWUID"            "" "Delete test user"
t GET "/Users"                    "" "Users after cleanup"

echo ""
echo "============================================"
echo "  LIBRARY"
echo "============================================"
t GET "/Library/MediaFolders"     "" "MediaFolders"
t GET "/Library/VirtualFolders"   "" "VirtualFolders"
t GET "/Library/PhysicalPaths"    "" "PhysicalPaths"
t GET "/Libraries/AvailableOptions" "" "AvailableOptions"

echo ""
echo "============================================"
echo "  ITEMS — READ"
echo "============================================"
t GET "/Items"                    "" "All Items"
t GET "/Items?IncludeItemTypes=Movie" "" "Movies only"
t GET "/Items?IncludeItemTypes=Episode" "" "Episodes only"
t GET "/Items?IncludeItemTypes=Audio" "" "Audio only"
t GET "/Items/Counts"             "" "Item Counts"
t GET "/Items?SortBy=ProductionYear&SortOrder=Descending&IncludeItemTypes=Movie" "" "Movies by year desc"
t GET "/UserViews"                "" "UserViews"
t GET "/Items/test-id/Ancestors"  "" "Ancestors"
t GET "/Items/test-id/ThemeSongs" "" "ThemeSongs"

echo ""
echo "============================================"
echo "  ITEMS — DETAIL"
echo "============================================"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/movie-1" "" "Movie detail (The Matrix)"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/movie-3" "" "Movie detail (Interstellar)"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/tv-bb" "" "Series detail (Breaking Bad)"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/music-album-1" "" "Album detail"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/music-track-1" "" "Track detail"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/Latest" "" "Latest items"
t GET "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/Resume" "" "Resume items"
t GET "/Items/movie-1/ExternalIdInfos" "" "External IDs"
t GET "/Items/movie-1/Images"    "" "Images list"
t GET "/Items/movie-1/Subtitles" "" "Subtitles"

echo ""
echo "============================================"
echo "  SHOWS"
echo "============================================"
t GET "/Shows/tv-bb/Episodes"    "" "Breaking Bad episodes"
t GET "/Shows/tv-bb/Seasons"     "" "Breaking Bad seasons"
t GET "/Shows/tv-st/Episodes"    "" "Stranger Things episodes"
t GET "/Shows/NextUp"            "" "Next Up"
t GET "/Shows/Missing"           "" "Missing episodes"
t GET "/Shows/Upcoming"          "" "Upcoming"

echo ""
echo "============================================"
echo "  FILTERS"
echo "============================================"
t GET "/Genres"                   "" "Genres"
t GET "/Persons"                  "" "Persons"
t GET "/Studios"                  "" "Studios"
t GET "/Tags"                     "" "Tags"
t GET "/Years"                    "" "Years"
t GET "/OfficialRatings"          "" "OfficialRatings"
t GET "/Containers"               "" "Containers"
t GET "/VideoCodecs"              "" "VideoCodecs"
t GET "/ExtendedVideoTypes"       "" "ExtendedVideoTypes"

echo ""
echo "============================================"
echo "  PERSONS"
echo "============================================"
t GET "/Persons/Christopher%20Nolan" "" "Person: Christopher Nolan"
t GET "/Persons/Christopher%20Nolan/Items" "" "Person items: Nolan"
t GET "/Persons/Keanu%20Reeves"     "" "Person: Keanu Reeves"

echo ""
echo "============================================"
echo "  SEARCH"
echo "============================================"
t GET "/Search/Hints?searchTerm=Matrix" "" "Search: Matrix"

echo ""
echo "============================================"
echo "  DLNA"
echo "============================================"
t GET "/Dlna/ProfileInfos"        "" "DLNA profiles"
t GET "/Dlna/Profiles/Default"    "" "DLNA default"
t GET "/Dlna/Profiles/default"    "" "DLNA default lower"
t GET "/description.xml"          "" "SSDP description"

echo ""
echo "============================================"
echo "  ENVIRONMENT"
echo "============================================"
t GET "/Environment/DefaultDirectoryBrowser" "" "DefaultDirBrowser"
t GET "/Environment/Drives"       "" "Drives"
t GET "/Environment/ParentPath?path=%2Ftmp" "" "ParentPath /tmp"

echo ""
echo "============================================"
echo "  PLAYBACK"
echo "============================================"
t POST "/Sessions/Playing"       '{"ItemId":"movie-1","PlayMethod":"DirectPlay","CanSeek":true}' "Start playback"
t POST "/Sessions/Playing/Progress" '{"ItemId":"movie-1","PlayMethod":"DirectPlay","PositionTicks":10000000,"IsPaused":false}' "Progress"
t POST "/Sessions/Playing/Stopped" '{"ItemId":"movie-1","PositionTicks":50000000}' "Stop playback"
t POST "/Sessions/Capabilities"  '{"PlayableMediaTypes":["Audio","Video"],"SupportsMediaControl":true,"SupportedCommands":["Play","Pause"]}' "Capabilities"
t POST "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/PlayedItems/movie-2" '{}' "Mark played"
t POST "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/PlayedItems/movie-2/Delete" '{}' "Mark unplayed"
t POST "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/FavoriteItems/movie-2" '{}' "Favorite"
t POST "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/FavoriteItems/movie-2/Delete" '{}' "Unfavorite"
t POST "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/movie-2/Rating" '{"Likes":true}' "Rate item"
t DEL "/Users/57b1e08d-ee2e-537e-ba71-9fe4537f9328/Items/movie-2/Rating" '' "Delete rating"

echo ""
echo "============================================"
echo "  SESSIONS"
echo "============================================"
t GET "/Sessions"                 "" "Active sessions"

echo ""
echo "============================================"
echo "  COLLECTIONS & PLAYLISTS"
echo "============================================"
t POST "/Collections?name=TestCollection" "" "Create collection"
# Get collection ID for add/remove
CID=$(curl -sS "$BASE/Collections?api_key=$TOKEN&name=TestCollection" | grep -o '"Id":"[^"]*"' | cut -d'"' -f4)
t POST "/Collections/$CID/Items" '{"Ids":["movie-1","movie-2"]}' "Add items to collection"
t DEL "/Collections/$CID/Items"  '{"Ids":["movie-2"]}' "Remove item from collection"

t POST "/Playlists"              '{"Name":"TestPlay","Ids":["music-track-1","music-track-2"]}' "Create playlist"
PID=$(curl -sS -X POST "$BASE/Playlists?api_key=$TOKEN" -H "Content-Type: application/json" -d '{"Name":"TestPlay","Ids":["music-track-1","music-track-2"]}' | grep -o '"Id":"[^"]*"' | head -1 | cut -d'"' -f4)
t GET "/Playlists/$PID"          "" "Get playlist"
t GET "/Playlists/$PID/Items"    "" "Playlist items"
t POST "/Playlists/$PID"         '{"Name":"UpdatedPlay","Ids":["music-track-3"]}' "Update playlist"

echo ""
echo "============================================"
echo "  DISPLAY PREFERENCES"
echo "============================================"
t GET "/DisplayPreferences/pref-movies" "" "Get prefs"
t POST "/DisplayPreferences/new-pref" '{"Id":"new-pref","ViewType":"List","SortBy":"DateCreated","SortOrder":"Descending"}' "Create prefs"

echo ""
echo "============================================"
echo "  SYSTEM CONFIGURATION"
echo "============================================"
t GET "/System/Configuration/MetadataOptions/Default" "" "DefaultMetadataOptions"
t GET "/System/Configuration/server_name" "" "Named config"
t POST "/System/Configuration/server_name" '{"ServerName":"jellyfin-rs"}' "Update named config"

echo ""
echo "============================================"
echo "  STUBS (should return 200/204)"
echo "============================================"
t GET "/Plugins"                  "" "Plugins"
t GET "/Packages"                 "" "Packages"
t GET "/Repositories"             "" "Repositories"
t GET "/Channels"                 "" "Channels"
t GET "/Auth/Providers"           "" "Auth Providers"
t GET "/Auth/PasswordResetProviders" "" "PasswordReset"
t GET "/Artists"                  "" "Artists"
t GET "/Artists/AlbumArtists"     "" "AlbumArtists"
t GET "/Items/Filters"            "" "Items/Filters"
t GET "/Items/Suggestions"        "" "Suggestions"
t GET "/MusicGenres"              "" "MusicGenres"
t GET "/LiveTv/Info"              "" "LiveTV Info"
t GET "/Movies/Recommendations"   "" "Recommendations"

echo ""
echo "============================================"
echo "  SUMMARY: $PASS passed, $WARN warnings, $FAIL failed"
echo "============================================"
