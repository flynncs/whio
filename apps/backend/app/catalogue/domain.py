from dataclasses import dataclass
from enum import StrEnum
from uuid import UUID


class ProviderId(StrEnum):
    YOUTUBE_MUSIC = "youtube_music"
    NAVIDROME = "navidrome"
    JELLYFIN = "jellyfin"
    LOCAL_FILE = "local_file"
    MONOCHROME = "monochrome"
    YOUTUBE = "youtube"


@dataclass(frozen=True)
class CatalogueResult:
    provider: ProviderId
    external_id: str
    title: str
    artists: tuple[str, ...]
    duration_seconds: int | None = None


@dataclass(frozen=True)
class Track:
    id: UUID
    title: str
    artists: tuple[str, ...]
    duration_seconds: int | None
    provisional: bool = True


@dataclass(frozen=True)
class SourceIdentity:
    provider: ProviderId
    external_id: str
    upstream_server_id: UUID | None = None
