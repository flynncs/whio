from dataclasses import dataclass


@dataclass(frozen=True)
class CatalogueSearchResult:
    provider: str
    external_id: str
    title: str
    artists: list[str]
    duration_seconds: int | None = None
