use super::CatalogueCandidate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedAlbum {
    candidates: Vec<CatalogueCandidate>,
    reported_count: Option<u32>,
}

impl FetchedAlbum {
    pub fn new(candidates: Vec<CatalogueCandidate>, reported_count: Option<u32>) -> Self {
        Self {
            candidates,
            reported_count,
        }
    }

    pub fn candidates(&self) -> &[CatalogueCandidate] {
        &self.candidates
    }

    pub fn into_candidates(self) -> Vec<CatalogueCandidate> {
        self.candidates
    }

    pub fn reported_count(&self) -> Option<u32> {
        self.reported_count
    }
}
