from pydantic import BaseModel, ConfigDict, Field


class YtDlpAudioPayload(BaseModel):
    """The subset of raw yt-dlp JSON consumed by the YouTube Music adapter."""

    model_config = ConfigDict(extra="ignore")

    url: str = Field(min_length=1)
    http_headers: dict[str, str] = Field(default_factory=dict)
    acodec: str | None = None
    abr: float | None = None
    duration: float | None = None
