from typing import Annotated

from fastapi import APIRouter, Depends
from ytmusicapi import YTMusic

from app.generated.resolver_v1 import (
    CatalogueCandidatesResponse,
    CatalogueEntityRequest,
    CatalogueSearchRequest,
    CatalogueSearchResponse,
)
from app.youtube.catalogue import YouTubeMusicCatalogue

catalogue_router = APIRouter(prefix="/v1/catalogue")


# todo: make this a singleton
def get_catalogue() -> YouTubeMusicCatalogue:
    return YouTubeMusicCatalogue(YTMusic())


@catalogue_router.post(
    "/search",
    response_model=CatalogueSearchResponse,
    response_model_exclude_none=True,
)
def search_catalogue(
    request: CatalogueSearchRequest,
    catalogue: Annotated[YouTubeMusicCatalogue, Depends(get_catalogue)],
) -> CatalogueSearchResponse:
    return catalogue.search(request.query, request.limit, request.offset)


@catalogue_router.post(
    "/album",
    response_model=CatalogueCandidatesResponse,
    response_model_exclude_none=True,
)
def fetch_catalogue_album(
    request: CatalogueEntityRequest,
    catalogue: Annotated[YouTubeMusicCatalogue, Depends(get_catalogue)],
) -> CatalogueCandidatesResponse:
    return catalogue.fetch_album(request.source)
