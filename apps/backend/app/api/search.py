from fastapi import APIRouter, Query

from app.integrations.youtube_music import search_songs

router = APIRouter(prefix="/search", tags=["YouTube Music"])


@router.get("")
def search(query: str = Query(min_length=1)):
    return {"results": search_songs(query)}
