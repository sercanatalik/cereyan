//! External identifiers are UUIDv7 values stored as 16-byte blobs so that
//! index inserts stay append-only.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(value_type = String))]
pub struct Id(pub Uuid);

impl Id {
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Id> {
        Uuid::from_slice(bytes).ok().map(Id)
    }

    pub fn parse(text: &str) -> Option<Id> {
        Uuid::parse_str(text).ok().map(Id)
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Debug for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

/// A new time-ordered identifier.
pub fn new_id() -> Id {
    Id(Uuid::now_v7())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_time_ordered() {
        let a = new_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = new_id();
        assert!(b > a);
        assert!(b.as_bytes() > a.as_bytes());
    }

    #[test]
    fn round_trips_through_bytes() {
        let a = new_id();
        assert_eq!(Id::from_bytes(a.as_bytes()), Some(a));
        assert_eq!(Id::parse(&a.to_string()), Some(a));
    }
}
