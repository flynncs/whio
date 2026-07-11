from dataclasses import dataclass
from uuid import UUID


@dataclass(frozen=True)
class CatalogueSearchResult:
    provider: str
    external_id: str
    title: str
    artists: list[str]
    duration_seconds: int | None = None


@dataclass(frozen=True)
class Recording:
    id: UUID
    title: str
    artists: tuple[str, ...]
    duration_seconds: int | None
    provisional: bool = True


@dataclass(frozen=True)
class SourceReference:
    recording_id: UUID
    provider: str
    external_id: str
    upstream_server_id: UUID | None = None
