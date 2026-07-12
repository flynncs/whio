from typing import Annotated

from fastapi import Depends, Request

from app.composition import Application
from app.playback.service import PlaybackService
from app.services.search import SearchService


def get_application(request: Request) -> Application:
    application = getattr(request.app.state, "application", None)

    if not isinstance(application, Application):
        raise RuntimeError("Application is not configured")

    return application


def get_search_service(
    application: Annotated[Application, Depends(get_application)],
) -> SearchService:
    return application.search_service


def get_playback_service(
    application: Annotated[Application, Depends(get_application)],
) -> PlaybackService:
    return application.playback_service
