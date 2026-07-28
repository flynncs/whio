from typing import Annotated

from fastapi import APIRouter, Depends
from ytmusicapi import YTMusic

from app.generated.resolver_v1 import (
    CatalogueSearchRequest,
    CatalogueSearchResponse,
)
from app.youtube.catalogue import YouTubeMusicCatalogue

catalogue_router = APIRouter(prefix="/v1/catalogue")


# todo: make this a singleton
def get_catalogue() -> YouTubeMusicCatalogue:
    return YouTubeMusicCatalogue(YTMusic())


@catalogue_router.post("/search", response_model=CatalogueSearchResponse)
def search_catalogue(
    request: CatalogueSearchRequest,
    catalogue: Annotated[YouTubeMusicCatalogue, Depends(get_catalogue)],
) -> CatalogueSearchResponse:
    results = catalogue.search(request.query, request.limit)

    return CatalogueSearchResponse(results=results)
