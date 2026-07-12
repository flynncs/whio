from fastapi import Request

from app.catalogue.application import SearchCatalogue
from app.playback.application import ResolveTrackPlayback


def get_search_catalogue(request: Request) -> SearchCatalogue:
    application = getattr(request.app.state, "application", None)
    service = getattr(application, "search_catalogue", None)
    if not isinstance(service, SearchCatalogue):
        raise RuntimeError("Application is not configured")

    return service


def get_resolve_track_playback(request: Request) -> ResolveTrackPlayback:
    application = getattr(request.app.state, "application", None)
    use_case = getattr(application, "resolve_track_playback", None)
    if not isinstance(use_case, ResolveTrackPlayback):
        raise RuntimeError("Application is not configured")

    return use_case
