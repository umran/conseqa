use serde::{Deserialize, Serialize};

use crate::spec::{FieldPath, Id, ResultType};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestInput {
    pub schema: Id,

    /// Identity of one logical request at this boundary.
    pub identity: RequestIdentity,

    /// The `Result<Ok, Err>` contract a request through this input
    /// returns.
    ///
    /// The contract belongs to the input rather than the operation: an
    /// operation may expose several request inputs, and a request
    /// effect already targets one specific input, from which it
    /// inherits this contract. Subscription inputs have no synchronous
    /// result.
    pub result: ResultType,
}

/// Where the identity of one logical request lives in the payload.
///
/// This is an implementation guarantee about the request boundary, not
/// a requirement and not a mechanism: it fixes what the payload of a
/// logical request is, deduplicates nothing, and does not by itself
/// discharge any idempotency obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestIdentity {
    /// No fact relates two requests sharing any field values. Distinct
    /// attempts may present arbitrarily different payloads under equal
    /// keys.
    Unspecified,

    /// Any two requests arriving at this input whose values at the
    /// declared identity fields are equal present equal payloads, at
    /// the granularity of the modeled schema: the payload is a
    /// function of its identity fields.
    ///
    /// The canonical conforming implementations are a boundary that
    /// rejects a retry whose payload disagrees with the original
    /// request under the same identity, and a caller contract strong
    /// enough to stand in a proof. A rejected conflicting request is
    /// not an admitted invocation, so rejection preserves the
    /// guarantee.
    Keyed(RequestIdentityKey),
}

/// The identity fields of a keyed request boundary, named so that
/// `deny_unknown_fields` applies through the tagged enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestIdentityKey {
    pub fields: Vec<FieldPath>,
}
