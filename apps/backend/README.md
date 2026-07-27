# existing python backend

this is the original FastAPI proof of concept.

it currently proves:

- youtube music search
- yt-dlp playback resolution
- proxy streaming with range support
- the basic lumen track/source split

the core is moving to rust. don't delete this app yet: it is the behaviour reference for the rust vertical slice, and its youtube-specific code will become the first optional resolver service.

new core domain work should go into the rust app once that exists. changes here should mainly keep the poc working or help extract the youtube resolver cleanly.

## setup

from `apps/backend`:

```sh
uv sync
uv run python -m unittest discover -s tests -p '*_test.py'
```

run it with:

```sh
uv run uvicorn app.main:app --reload
```

## endpoints

```text
GET /health
GET /search?query={query}&limit={1..25}
GET /playback/stream/{track_id}
```

`limit` defaults to 5. playback takes a lumen track id, not a youtube id.

## examples

search:

```json
{
  "results": [
    {
      "id": "00000000-0000-0000-0000-000000000001",
      "title": "Instant Crush",
      "artists": ["Daft Punk"],
      "duration_seconds": 337
    }
  ]
}
```

the internal playback resolver returns this shape before streaming starts:

```json
{
  "provider": "youtube_music",
  "external_id": "example-source-id",
  "url": "https://media.example.test/audio",
  "headers": {"User-Agent": "example"},
  "codec": "opus",
  "bitrate": 128.5,
  "duration": 337.25
}
```

these are fake values. don't commit real signed urls, cookies, credentials or resolver headers.

## streaming behaviour to preserve

- forwards the client's `Range` header upstream
- returns the upstream status code
- forwards `Content-Type`, `Content-Length`, `Content-Range`, `Accept-Ranges`, `ETag` and `Last-Modified`
- streams in 64 KiB chunks with up to 8 MiB prefetched
- cancels the producer when the downstream stream ends
- closes the upstream response and HTTP client after the response finishes
- resolves playback before starting the downstream response
- returns safe JSON errors without internal exception details
- accepts an `X-Request-ID` or creates one, then returns it in the response
