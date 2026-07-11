import asyncio
import logging
from collections.abc import AsyncIterator
from contextlib import suppress

import httpx
from fastapi.responses import StreamingResponse
from starlette.background import BackgroundTask

from app.playback.contracts import ResolvedPlaybackSource

logger = logging.getLogger("uvicorn.error")

CHUNK_SIZE = 64 * 1024
PREFETCH_BYTES = 8 * 1024 * 1024
PREFETCH_CHUNKS = PREFETCH_BYTES // CHUNK_SIZE


async def stream_source(
    source: ResolvedPlaybackSource, range_header: str | None
) -> StreamingResponse:
    upstream_headers = dict(source.headers)

    if range_header:
        upstream_headers["Range"] = range_header

    client = httpx.AsyncClient(
        follow_redirects=True,
        timeout=None,
    )

    upstream_request = client.build_request("GET", source.url, headers=upstream_headers)
    upstream_response = await client.send(upstream_request, stream=True)

    logger.info(
        "Upstream opened: status=%s content_length=%s range=%s",
        upstream_response.status_code,
        upstream_response.headers.get("content-length"),
        range_header,
    )

    forwarded_headers = {}

    for header in [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "etag",
        "last-modified",
    ]:
        if header in upstream_response.headers:
            forwarded_headers[header] = upstream_response.headers[header]

    return StreamingResponse(
        _prefetch_stream(upstream_response),
        status_code=upstream_response.status_code,
        headers=forwarded_headers,
        background=BackgroundTask(_close_upstream, upstream_response, client),
    )


async def _prefetch_stream(
    response: httpx.Response,
) -> AsyncIterator[bytes]:
    queue: asyncio.Queue[bytes | None] = asyncio.Queue(
        maxsize=PREFETCH_CHUNKS,
    )
    error: Exception | None = None

    async def produce() -> None:
        nonlocal error

        bytes_read = 0
        next_log_at = 1024 * 1024
        logger.info("Upstream prefetch started")

        try:
            async for chunk in response.aiter_raw(
                chunk_size=CHUNK_SIZE,
            ):
                await queue.put(chunk)
                bytes_read += len(chunk)

                if bytes_read >= next_log_at:
                    logger.info(
                        "Prefetched %s MiB; queue=%s/%s chunks",
                        round(bytes_read / 1024 / 1024, 1),
                        queue.qsize(),
                        PREFETCH_CHUNKS,
                    )
                    next_log_at += 1024 * 1024
        except Exception as exception:
            logger.exception("Upstream prefetch failed after %s bytes", bytes_read)
            error = exception
        else:
            logger.info("Upstream prefetch finished: %s bytes", bytes_read)
        finally:
            await queue.put(None)

    producer = asyncio.create_task(produce())

    try:
        while True:
            chunk = await queue.get()

            if chunk is None:
                break

            yield chunk

        if error is not None:
            raise error
    finally:
        if not producer.done():
            producer.cancel()

        with suppress(asyncio.CancelledError):
            await producer


async def _close_upstream(response: httpx.Response, client: httpx.AsyncClient) -> None:
    await response.aclose()
    await client.aclose()
