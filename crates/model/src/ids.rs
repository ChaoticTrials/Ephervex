//! Typed identifiers.
//!
//! Instances, servers, and operations all use UUIDs. Wrapping them in distinct
//! newtypes means the compiler rejects passing a server id where an instance id
//! is expected, which is a mistake that is otherwise easy to make in registry
//! and launch code.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID, e.g. when reading from the database.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

typed_id!(
    /// Identifies a client instance. Also names its directory under the
    /// temporary or persistent instance root.
    InstanceId
);

typed_id!(
    /// Identifies a server instance.
    ServerId
);

typed_id!(
    /// Identifies a long-running background operation so its progress events
    /// and log lines can be correlated.
    OperationId
);

typed_id!(
    /// Identifies one local safety snapshot inside an installed host.
    SnapshotId
);

typed_id!(
    /// Identifies a user-defined instance group, so renaming the group never
    /// touches the instances assigned to it.
    GroupId
);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn ids_round_trip_through_their_string_form() {
        let id = InstanceId::new();
        let parsed: InstanceId = id.to_string().parse().expect("id should re-parse");
        assert_eq!(id, parsed);
    }

    #[test]
    fn distinct_ids_are_generated() {
        assert_ne!(InstanceId::new(), InstanceId::new());
    }

    #[test]
    fn ids_serialize_as_a_bare_uuid_string() {
        let id = ServerId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{id}\""));
    }
}
