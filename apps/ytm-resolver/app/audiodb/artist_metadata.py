from collections.abc import Mapping
from datetime import UTC, datetime
from typing import Any

from app.audiodb.client import AudioDbClient
from app.generated.resolver_v1 import (
    Attribution,
    CandidateSource,
    ErrorCode,
    ImageDescriptor,
    LicenceDescriptor,
    MatchBasis,
    SourceIdentity,
)
from app.metadata.document import (
    BIOGRAPHY_CONTENT_LIMIT_BYTES,
    FIELD_TOO_LARGE_MESSAGE,
    ArtistDocument,
    DocumentError,
    FieldCandidate,
    FieldError,
    exceeds_cap,
    select_requested_fields,
)

MISSING = object()

BIOGRAPHY_CONTENT_FIELD = "biography.content"
BIOGRAPHY_LANGUAGE_FIELD = "biography.language"
PORTRAIT_FIELD = "image.portrait"
WIDE_FIELD = "image.wide"
PORTRAIT_ROLE = "portrait"
WIDE_ROLE = "wide"

COLLECTION_MEMBER = "artists"
PORTRAIT_MEMBERS = ("strArtistThumb",)
WIDE_MEMBERS = (
    "strArtistWideThumb",
    "strArtistFanart",
    "strArtistFanart2",
    "strArtistFanart3",
    "strArtistFanart4",
)

BIOGRAPHY_MEMBERS = (
    "strBiographyEN",
    "strBiography",
    "strBiographyDE",
    "strBiographyFR",
    "strBiographyCN",
    "strBiographyIT",
    "strBiographyJP",
    "strBiographyRU",
    "strBiographyES",
    "strBiographyPT",
    "strBiographySE",
    "strBiographyNL",
    "strBiographyHU",
    "strBiographyNO",
    "strBiographyIL",
    "strBiographyPL",
)

BIOGRAPHY_LANGUAGES = {
    "strBiographyEN": "en",
    "strBiographyDE": "de",
    "strBiographyFR": "fr",
    "strBiographyCN": "zh",
    "strBiographyIT": "it",
    "strBiographyJP": "ja",
    "strBiographyRU": "ru",
    "strBiographyES": "es",
    "strBiographyPT": "pt",
    "strBiographySE": "sv",
    "strBiographyNL": "nl",
    "strBiographyHU": "hu",
    "strBiographyNO": "no",
    "strBiographyIL": "he",
    "strBiographyPL": "pl",
}

CREATIVE_COMMONS_MEMBER = "strCreativeCommons"
ARTIST_ID_MEMBER = "idArtist"
MBID_MEMBER = "strMusicBrainzID"
AUDIODB_PROVIDER = "theaudiodb"

AUDIODB_TERMS_ID = "theaudiodb.terms"
AUDIODB_TERMS_NAME = "TheAudioDB Terms of Use"
AUDIODB_TERMS_URL = "https://www.theaudiodb.com/docs_terms_of_use.php"
AUDIODB_TERMS_BASIS = (
    "TheAudioDB grants copying of API content through the official endpoints; "
    "artwork copyright stays with its owner"
)

AUDIODB_CREATIVE_COMMONS_ID = "theaudiodb.creativecommons"
AUDIODB_CREATIVE_COMMONS_NAME = "TheAudioDB Creative Commons artwork"
AUDIODB_CREATIVE_COMMONS_BASIS = (
    "The artist artwork carries TheAudioDB's strCreativeCommons signal"
)

AUDIODB_ATTRIBUTION_TEXT = "Data provided by TheAudioDB"
AUDIODB_ARTIST_PAGE = "https://www.theaudiodb.com/artist/{artist_id}"


class AudioDbArtistMetadata:
    _client: AudioDbClient

    def __init__(self, client: AudioDbClient) -> None:
        self._client = client

    def fetch_artist_document(
        self,
        source: SourceIdentity,
        language: str | None,
        fields: list[str] | None,
    ) -> ArtistDocument:
        retrieved_at = datetime.now(UTC)

        artist = parse_artist(self._client.fetch_artist_by_mbid(source.external_id))
        candidates, errors = parse_candidates(source, artist, language)

        return ArtistDocument(
            address=source,
            retrieved_at=retrieved_at,
            candidates=select_requested_fields(candidates, fields),
            errors=select_requested_fields(errors, fields),
        )


def parse_artist(payload: Any) -> Mapping[str, Any]:
    if not isinstance(payload, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    artists = payload.get(COLLECTION_MEMBER, MISSING)
    if artists is None:
        raise DocumentError(ErrorCode.source_not_found, False)
    if not isinstance(artists, list):
        raise DocumentError(ErrorCode.resolution_failed, False)
    if not artists:
        raise DocumentError(ErrorCode.source_not_found, False)

    artist = artists[0]
    if not isinstance(artist, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    return artist


def producing_source(artist: Mapping[str, Any]) -> CandidateSource:
    external_id = artist.get(MBID_MEMBER)
    if isinstance(external_id, str) and external_id.strip():
        return CandidateSource(
            provider_id=AUDIODB_PROVIDER,
            external_id=external_id.strip(),
            match_basis=MatchBasis.established_source,
            url=audiodb_artist_url(artist),
        )
    raise DocumentError(ErrorCode.resolution_failed, False)


def audiodb_artist_url(artist: Mapping[str, Any]) -> str | None:
    artist_id = artist.get(ARTIST_ID_MEMBER)
    if isinstance(artist_id, str) and artist_id.strip():
        return AUDIODB_ARTIST_PAGE.format(artist_id=artist_id.strip())
    return None


def parse_candidates(
    source: SourceIdentity,
    artist: Mapping[str, Any],
    language: str | None,
) -> tuple[list[FieldCandidate], list[FieldError]]:
    terms = audiodb_terms_licence()
    producing = producing_source(artist)
    artwork = audiodb_artwork_licence(
        is_creative_commons(artist.get(CREATIVE_COMMONS_MEMBER))
    )
    attribution = parse_attribution(artist)

    candidates: list[FieldCandidate] = []
    errors: list[FieldError] = []

    member, text = select_biography(artist, language)
    if text is not None:
        if exceeds_cap(text, BIOGRAPHY_CONTENT_LIMIT_BYTES):
            errors.append(
                FieldError(
                    field=BIOGRAPHY_CONTENT_FIELD,
                    code=ErrorCode.resolution_failed,
                    retryable=False,
                    message=FIELD_TOO_LARGE_MESSAGE,
                )
            )
        else:
            candidates.append(
                FieldCandidate(
                    field=BIOGRAPHY_CONTENT_FIELD,
                    source=producing,
                    licence=terms,
                    text=text,
                    attribution=attribution,
                )
            )
        declared = BIOGRAPHY_LANGUAGES.get(member)
        if declared is not None:
            candidates.append(
                FieldCandidate(
                    field=BIOGRAPHY_LANGUAGE_FIELD,
                    source=producing,
                    licence=terms,
                    language=declared,
                    attribution=attribution,
                )
            )

    for field, members, role in (
        (PORTRAIT_FIELD, PORTRAIT_MEMBERS, PORTRAIT_ROLE),
        (WIDE_FIELD, WIDE_MEMBERS, WIDE_ROLE),
    ):
        candidate, error = parse_image_field(
            producing, artist, members, field, role, artwork, attribution
        )
        if candidate is not None:
            candidates.append(candidate)
        if error is not None:
            errors.append(error)

    return candidates, errors


def parse_image_field(
    producing: CandidateSource,
    artist: Mapping[str, Any],
    members: tuple[str, ...],
    field: str,
    role: str,
    licence: LicenceDescriptor,
    attribution: Attribution,
) -> tuple[FieldCandidate | None, FieldError | None]:
    for member in members:
        value = artist.get(member, MISSING)
        if value is MISSING or value is None:
            continue
        if not isinstance(value, str):
            return None, FieldError(
                field=field,
                code=ErrorCode.resolution_failed,
                retryable=False,
            )
        if not value.strip().startswith(("http://", "https://")):
            continue
        return (
            FieldCandidate(
                field=field,
                source=producing,
                licence=licence,
                image=ImageDescriptor(role=role, reference=value.strip()),
                attribution=attribution,
            ),
            None,
        )
    return None, None


def select_biography(
    artist: Mapping[str, Any], language: str | None
) -> tuple[str | None, str | None]:
    for member in biography_member_order(language):
        value = artist.get(member, MISSING)
        if isinstance(value, str) and value.strip():
            return member, value
    return None, None


def biography_member_order(language: str | None) -> tuple[str, ...]:
    for member, declared in BIOGRAPHY_LANGUAGES.items():
        if language is not None and declared == language:
            return (member, *BIOGRAPHY_MEMBERS)
    return BIOGRAPHY_MEMBERS


def is_creative_commons(value: Any) -> bool:
    if value is True:
        return True
    if isinstance(value, str) and value.strip().lower() in {"true", "yes"}:
        return True
    return False


def parse_attribution(artist: Mapping[str, Any]) -> Attribution:
    artist_id = artist.get(ARTIST_ID_MEMBER)
    url = None
    if isinstance(artist_id, str) and artist_id.strip():
        url = AUDIODB_ARTIST_PAGE.format(artist_id=artist_id.strip())
    return Attribution(text=AUDIODB_ATTRIBUTION_TEXT, url=url)


def audiodb_artwork_licence(creative_commons: bool) -> LicenceDescriptor:
    if creative_commons:
        return audiodb_creative_commons_licence()
    return audiodb_terms_licence()


def audiodb_terms_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=AUDIODB_TERMS_ID,
        name=AUDIODB_TERMS_NAME,
        url=AUDIODB_TERMS_URL,
        attribution_required=True,
        redistribution_allowed=False,
        cache_allowed=True,
        share_alike=False,
        non_commercial=False,
        terms_basis=AUDIODB_TERMS_BASIS,
        uncertainty=True,
    )


def audiodb_creative_commons_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=AUDIODB_CREATIVE_COMMONS_ID,
        name=AUDIODB_CREATIVE_COMMONS_NAME,
        url=AUDIODB_TERMS_URL,
        attribution_required=True,
        redistribution_allowed=True,
        cache_allowed=True,
        share_alike=False,
        non_commercial=False,
        terms_basis=AUDIODB_CREATIVE_COMMONS_BASIS,
        uncertainty=False,
    )
