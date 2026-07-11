from typing import Annotated

from fastapi import APIRouter, Query

from app.api.schemas.search import SearchQuery, SearchResponse, SearchResult
from app.integrations.youtube.catalogue import search_songs

router = APIRouter(prefix="/search", tags=["YouTube Music"])


@router.get("", response_model=SearchResponse)
def search(params: Annotated[SearchQuery, Query()]) -> SearchResponse:
    return SearchResponse(
        results=[
            SearchResult(
                provider=result.provider,
                external_id=result.external_id,
                title=result.title,
                artists=result.artists,
                duration_seconds=result.duration_seconds,
            )
            for result in search_songs(params.query, params.limit)
        ]
    )
