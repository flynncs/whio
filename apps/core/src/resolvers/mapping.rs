use whio_resolver_api::models::CatalogueCandidate as CatalogueCandidateDto;

use crate::catalogue::{
    CatalogueCandidate, ProviderId, SourceIdentity, TrackMetadata, ValidationError,
};

impl TryFrom<CatalogueCandidateDto> for CatalogueCandidate {
    type Error = ValidationError;

    fn try_from(candidate: CatalogueCandidateDto) -> Result<Self, Self::Error> {
        let provider_id = ProviderId::new(candidate.source.provider_id)?;
        let source = SourceIdentity::new(provider_id, candidate.source.external_id)?;

        let duration_ms = candidate
            .duration_ms
            .flatten()
            .map(u64::try_from)
            .transpose()
            .map_err(|_| ValidationError::InvalidDuration)?;

        let metadata = TrackMetadata::new(candidate.title, candidate.artists, duration_ms)?;

        Ok(CatalogueCandidate::new(source, metadata))
    }
}

#[cfg(test)]
mod tests {
    use whio_resolver_api::models::SourceIdentity as SourceIdentityDto;

    use super::*;

    fn candidate() -> CatalogueCandidateDto {
        CatalogueCandidateDto {
            source: Box::new(SourceIdentityDto::new(
                "example_music".to_owned(),
                "example-source-id".to_owned(),
            )),
            title: "Instant Crush".to_owned(),
            artists: vec!["Daft Punk".to_owned()],
            duration_ms: Some(Some(337_000)),
        }
    }

    #[test]
    fn valid_dto_becomes_valid_domain_candidate() {
        let candidate = CatalogueCandidate::try_from(candidate()).unwrap();

        assert_eq!(candidate.source().provider_id().as_str(), "example_music");
        assert_eq!(candidate.source().external_id(), "example-source-id");
        assert_eq!(candidate.title(), "Instant Crush");
        assert_eq!(candidate.artists(), ["Daft Punk"]);
        assert_eq!(candidate.duration_ms(), Some(337_000));
    }

    #[test]
    fn invalid_dto_is_rejected_at_the_adapter_boundary() {
        let mut candidate = candidate();
        candidate.duration_ms = Some(Some(-1));

        assert!(matches!(
            CatalogueCandidate::try_from(candidate),
            Err(ValidationError::InvalidDuration)
        ));
    }
}
