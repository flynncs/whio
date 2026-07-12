from typing import Annotated

from fastapi import APIRouter, Depends, Query

from app.api.dependencies import get_search_service
from app.api.schemas.search import SearchQuery, SearchResponse, SearchResult
from app.services.search import SearchService

router = APIRouter(prefix="/search", tags=["YouTube Music"])


@router.get("", response_model=SearchResponse)
def search(
    params: Annotated[SearchQuery, Query()],
    service: Annotated[SearchService, Depends(get_search_service)],
) -> SearchResponse:
    tracks = service.search(query=params.query, limit=params.limit)

    return SearchResponse(
        results=[
            SearchResult(
                id=track.id,
                title=track.title,
                artists=list(track.artists),
                duration_seconds=track.duration_seconds,
            )
            for track in tracks
        ]
    )
