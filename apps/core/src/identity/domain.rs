use std::fmt;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(Uuid);

impl UserId {
    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub display_name: String,
}

#[derive(Debug, Clone)]
pub struct ApiKeyCredential {
    pub id: Uuid,
    pub user_id: UserId,
}

#[derive(Debug, Clone)]
pub struct AppPasswordCredential {
    pub id: Uuid,
    pub user_id: UserId,
    pub encrypted_secret: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub user_id: UserId,
}
