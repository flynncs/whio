from typing import Annotated

from fastapi import APIRouter, Depends, Query

from app.catalogue.application import SearchCatalogue
from app.delivery.http.dependencies import get_search_catalogue
from app.delivery.http.schemas.search import SearchQuery, SearchResponse, SearchResult

router = APIRouter(prefix="/search", tags=["YouTube Music"])


@router.get("", response_model=SearchResponse)
def search(
    params: Annotated[SearchQuery, Query()],
    service: Annotated[SearchCatalogue, Depends(get_search_catalogue)],
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
