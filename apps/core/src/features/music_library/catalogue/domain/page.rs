use super::SearchOffset;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CataloguePage<T> {
    items: Vec<T>,
    next_offset: Option<SearchOffset>,
}

impl<T> CataloguePage<T> {
    pub fn new(items: Vec<T>, next_offset: Option<SearchOffset>) -> Self {
        Self { items, next_offset }
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn next_offset(&self) -> Option<SearchOffset> {
        self.next_offset
    }

    pub fn into_parts(self) -> (Vec<T>, Option<SearchOffset>) {
        (self.items, self.next_offset)
    }
}
