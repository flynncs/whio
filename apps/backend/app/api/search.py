from typing import Annotated

from fastapi import APIRouter, Query

from app.api.schemas.search import SearchQuery, SearchResponse, SearchResult
from app.domain.catalogue_repository import InMemoryCatalogueRepository
from app.integrations.youtube.catalogue import YoutubeMusicCatalogueProvider
from app.services.search import SearchService

router = APIRouter(prefix="/search", tags=["YouTube Music"])

catalogue_repository = InMemoryCatalogueRepository()

search_service = SearchService(
    repository=catalogue_repository, providers=[YoutubeMusicCatalogueProvider()]
)


@router.get("", response_model=SearchResponse)
def search(params: Annotated[SearchQuery, Query()]) -> SearchResponse:
    recordings = search_service.search(query=params.query, limit=params.limit)

    return SearchResponse(
        results=[
            SearchResult(
                id=recording.id,
                title=recording.title,
                artists=list(recording.artists),
                duration_seconds=recording.duration_seconds,
            )
            for recording in recordings
        ]
    )
