use serde::{Deserialize, Serialize};

use crate::spec::ValueRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyKey {
    pub components: Vec<ValueRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyKeyPropagation {
    pub source: IdempotencyKey,
    pub target: IdempotencyKey,
}

/// A transaction's commit-deduplication guarantee — transaction-only.
///
/// `DeduplicatedBy` is the model's own keyed-commit construct,
/// `Commit(operation, transaction, K)`, load-bearing for route-B
/// artifact recovery (§17). External boundaries declare their facts
/// through `ExternalIdentity` / `ExternalIdempotency` /
/// `ExternalResultReplay` instead; this enum never appears on an
/// external effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdempotencyGuarantee {
    Unspecified,

    NotDeduplicated,

    DeduplicatedBy { key: IdempotencyKey },
}
