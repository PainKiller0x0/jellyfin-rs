# SmartStrm direct-play contract fixture

This directory contains a dependency-free local HTTP fixture for the part of the
playback contract that can be tested without a real Quark account:

- direct media responses support `HEAD`, `Accept-Ranges`, and `Content-Length`;
- `302` and `307` responses expose a usable `Location` header;
- a single `Range` request returns `206` and a matching `Content-Range`.

It is deliberately not a replacement for an end-to-end test against SmartStrm.
The next playback batch can point `JELLYFIN_RS_STRM_PUBLIC_BASE_URL` at this
fixture and exercise the actual server route without touching production.

Run it with:

```text
python -m unittest discover -s tests/smartstrm_contract -p "test_*.py"
```
