#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn end(&self) -> u64 {
        self.end
    }

    pub fn is_empty(&self) -> bool {
        self.start > self.end
    }

    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}
