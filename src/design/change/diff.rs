//! Declaration changes (§8 of the spec change analysis revision): what
//! changed in the architecture, each change attributed to exactly one
//! declaration.
//!
//! A declaration present only on head is added, one present only on
//! base is removed, and one present on both whose residuals differ is
//! modified, with a structural delta. Ids are identity: a renamed
//! declaration is a removal and an addition, and nothing pairs a
//! removal with an addition by similarity, file name, or any other
//! heuristic (§8.3). Authoring changes are listed apart, and never
//! count as semantic.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::analyzer::basis::DeclarationKey;

use super::delta::{DeltaEntry, delta};
use super::partition::{AuthoringItem, DeclarationLayer, Partition, layer};

/// How a declaration or an authoring item changed between the sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    Modified,
}

/// One changed declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationChange {
    pub declaration: DeclarationKey,
    pub layer: DeclarationLayer,
    pub change: Change,

    /// What moved inside a modified declaration's residual.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delta: Vec<DeltaEntry>,
}

/// One changed piece of authoring content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringChange {
    pub item: AuthoringItem,
    pub change: Change,
}

/// Every change between two partitions, in the order a diff lists them:
/// by layer, then member kind in §7.2's order, then id (§8.4); and the
/// authoring changes apart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationChanges {
    pub changes: Vec<DeclarationChange>,
    pub authoring: Vec<AuthoringChange>,
}

impl DeclarationChanges {
    /// Whether nothing semantic changed. Authoring changes do not count.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// The changes from `base` to `head`.
pub fn declaration_changes(base: &Partition, head: &Partition) -> DeclarationChanges {
    // `DeclarationKey` orders by member kind in §7.2's order, which is
    // already L0, then L1, then intent.
    let declarations: BTreeSet<&DeclarationKey> =
        base.members.keys().chain(head.members.keys()).collect();

    let changes = declarations
        .into_iter()
        .filter_map(|declaration| {
            let (change, delta) =
                match (base.members.get(declaration), head.members.get(declaration)) {
                    (None, Some(_)) => (Change::Added, Vec::new()),
                    (Some(_), None) => (Change::Removed, Vec::new()),
                    (Some(base), Some(head)) if base != head => {
                        (Change::Modified, delta(base, head))
                    }
                    (Some(_), Some(_)) | (None, None) => return None,
                };

            Some(DeclarationChange {
                declaration: declaration.clone(),
                layer: layer(declaration),
                change,
                delta,
            })
        })
        .collect();

    let items: BTreeSet<&AuthoringItem> =
        base.authoring.keys().chain(head.authoring.keys()).collect();

    let authoring = items
        .into_iter()
        .filter_map(|item| {
            let change = match (base.authoring.get(item), head.authoring.get(item)) {
                (None, Some(_)) => Change::Added,
                (Some(_), None) => Change::Removed,
                (Some(base), Some(head)) if base != head => Change::Modified,
                (Some(_), Some(_)) | (None, None) => return None,
            };

            Some(AuthoringChange {
                item: item.clone(),
                change,
            })
        })
        .collect();

    DeclarationChanges { changes, authoring }
}
