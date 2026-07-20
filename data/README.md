# IP geolocation data

`ip2region_v4.xdb` is copied from the ip2region project:

https://github.com/lionsoul2014/ip2region

The database is used for offline IPv4 geolocation in the admin playback map.
It is included so playback IP addresses do not need to be sent to any external
geolocation service. See `IP2REGION_LICENSE.md` for the upstream license.

To override the bundled database at runtime, set:

```sh
JELLYFIN_RS_IP2REGION_V4_XDB=/path/to/ip2region_v4.xdb
```
