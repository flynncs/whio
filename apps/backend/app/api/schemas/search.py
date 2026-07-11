from pydantic import BaseModel, ConfigDict, Field


class SearchQuery(BaseModel):
    model_config = ConfigDict(extra="forbid")

    query: str = Field(min_length=1)
    limit: int = Field(default=5, ge=1, le=25)


class SearchResult(BaseModel):
    provider: str
    external_id: str
    title: str
    artists: list[str]
    duration_seconds: int | None = None


class SearchResponse(BaseModel):
    results: list[SearchResult]
