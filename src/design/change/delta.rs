//! Structural deltas: a readable account of what moved inside a
//! modified declaration's residual (§8.2 of the spec change analysis
//! revision).
//!
//! A delta is presentation. No classification, gate, or explanation
//! reads one: a declaration is modified exactly when its residuals
//! differ, whatever its delta says.
//!
//! Maps align by key, and lists of elements carrying an `id` align by
//! id. Every other list aligns by the longest common subsequence of its
//! elements, so an inserted transaction step is reported as one
//! insertion rather than as a change to every step after it. Between
//! two aligned elements, the remaining elements of each side pair up by
//! position: a pair of the same kind is compared field by field, and a
//! pair of different kinds is one replacement, as an `update` turned
//! `compare_and_set` is. What is left over is added or removed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One change inside a residual: a value present on one side, the
/// other, or both with different contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaEntry {
    /// Where the change is, as the side it names writes it:
    /// `steps[3].compare[0].field`. An element whose index differs
    /// between the sides gives both, base then head: `steps[2→3]`.
    pub path: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Value>,
}

/// The structural delta from one residual to another: empty when they
/// are equal.
pub fn delta(base: &Value, head: &Value) -> Vec<DeltaEntry> {
    let mut entries = Vec::new();

    compare(base, head, "", &mut entries);

    entries
}

fn compare(base: &Value, head: &Value, path: &str, entries: &mut Vec<DeltaEntry>) {
    if base == head {
        return;
    }

    match (base, head) {
        (Value::Object(base), Value::Object(head)) => {
            let mut keys: Vec<&String> = base.keys().chain(head.keys()).collect();

            keys.sort();
            keys.dedup();

            for key in keys {
                let at = field(path, key);

                match (base.get(key), head.get(key)) {
                    (Some(base), Some(head)) => compare(base, head, &at, entries),
                    (Some(base), None) => entries.push(removed(at, base)),
                    (None, Some(head)) => entries.push(added(at, head)),
                    (None, None) => {}
                }
            }
        }

        (Value::Array(base), Value::Array(head)) => list(base, head, path, entries),

        _ => entries.push(DeltaEntry {
            path: path.to_string(),
            base: Some(base.clone()),
            head: Some(head.clone()),
        }),
    }
}

fn list(base: &[Value], head: &[Value], path: &str, entries: &mut Vec<DeltaEntry>) {
    if let (Some(base_ids), Some(head_ids)) = (ids(base), ids(head)) {
        return by_id(base, head, &base_ids, &head_ids, path, entries);
    }

    let mut i = 0;
    let mut j = 0;

    for (anchor_i, anchor_j) in common_subsequence(base, head)
        .into_iter()
        .chain([(base.len(), head.len())])
    {
        // The gap before this anchor: pair by position, then the rest.
        while i < anchor_i && j < anchor_j {
            let at = index(path, i, j);

            if same_kind(&base[i], &head[j]) {
                compare(&base[i], &head[j], &at, entries);
            } else {
                entries.push(DeltaEntry {
                    path: at,
                    base: Some(base[i].clone()),
                    head: Some(head[j].clone()),
                });
            }

            i += 1;
            j += 1;
        }

        for (k, removed_element) in base.iter().enumerate().take(anchor_i).skip(i) {
            entries.push(removed(index(path, k, k), removed_element));
        }

        for (k, added_element) in head.iter().enumerate().take(anchor_j).skip(j) {
            entries.push(added(index(path, k, k), added_element));
        }

        i = anchor_i + 1;
        j = anchor_j + 1;
    }
}

/// Aligns two lists whose elements all carry unique `id`s.
fn by_id(
    base: &[Value],
    head: &[Value],
    base_ids: &[&str],
    head_ids: &[&str],
    path: &str,
    entries: &mut Vec<DeltaEntry>,
) {
    for (i, id) in base_ids.iter().enumerate() {
        match head_ids.iter().position(|other| other == id) {
            Some(j) => compare(&base[i], &head[j], &index(path, i, j), entries),
            None => entries.push(removed(index(path, i, i), &base[i])),
        }
    }

    for (j, id) in head_ids.iter().enumerate() {
        if !base_ids.contains(id) {
            entries.push(added(index(path, j, j), &head[j]));
        }
    }
}

/// Every element's `id`, when every element is a map carrying a unique
/// string `id`.
fn ids(elements: &[Value]) -> Option<Vec<&str>> {
    let ids: Vec<&str> = elements
        .iter()
        .map(|element| element.get("id").and_then(Value::as_str))
        .collect::<Option<_>>()?;

    let mut unique = ids.clone();

    unique.sort_unstable();
    unique.dedup();

    (!ids.is_empty() && unique.len() == ids.len()).then_some(ids)
}

/// The index pairs of a longest common subsequence of equal elements.
fn common_subsequence(base: &[Value], head: &[Value]) -> Vec<(usize, usize)> {
    let (n, m) = (base.len(), head.len());

    // lengths[i][j]: the longest common subsequence of base[i..], head[j..].
    let mut lengths = vec![vec![0usize; m + 1]; n + 1];

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lengths[i][j] = if base[i] == head[j] {
                lengths[i + 1][j + 1] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }

    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);

    while i < n && j < m {
        if base[i] == head[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if lengths[i + 1][j] >= lengths[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }

    pairs
}

/// Whether two elements are worth comparing field by field: maps whose
/// `kind`s agree, or values of one type.
fn same_kind(base: &Value, head: &Value) -> bool {
    match (base, head) {
        (Value::Object(base), Value::Object(head)) => base.get("kind") == head.get("kind"),
        (Value::Array(_), Value::Array(_)) => true,
        _ => false,
    }
}

fn field(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn index(path: &str, base: usize, head: usize) -> String {
    if base == head {
        format!("{path}[{base}]")
    } else {
        format!("{path}[{base}→{head}]")
    }
}

fn added(path: String, head: &Value) -> DeltaEntry {
    DeltaEntry {
        path,
        base: None,
        head: Some(head.clone()),
    }
}

fn removed(path: String, base: &Value) -> DeltaEntry {
    DeltaEntry {
        path,
        base: Some(base.clone()),
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(entries: &[DeltaEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.path.as_str()).collect()
    }

    #[test]
    fn equal_residuals_have_no_delta() {
        let value = json!({ "isolation": "serializable", "steps": [{ "kind": "read" }] });

        assert!(delta(&value, &value).is_empty());
    }

    #[test]
    fn maps_align_by_key() {
        let entries = delta(
            &json!({ "isolation": "serializable", "data_model": "data.orders" }),
            &json!({ "isolation": "read_committed", "idempotency": { "kind": "not_deduplicated" } }),
        );

        assert_eq!(
            entries,
            vec![
                DeltaEntry {
                    path: "data_model".to_string(),
                    base: Some(json!("data.orders")),
                    head: None,
                },
                DeltaEntry {
                    path: "idempotency".to_string(),
                    base: None,
                    head: Some(json!({ "kind": "not_deduplicated" })),
                },
                DeltaEntry {
                    path: "isolation".to_string(),
                    base: Some(json!("serializable")),
                    head: Some(json!("read_committed")),
                },
            ]
        );
    }

    /// An inserted step is one insertion, not a change to every step
    /// after it.
    #[test]
    fn an_inserted_step_is_one_insertion() {
        let read = json!({ "kind": "read", "bind": "read.order" });
        let update = json!({ "kind": "update", "fields": ["status"] });
        let lock = json!({ "kind": "lock", "mode": "exclusive" });

        let entries = delta(
            &json!({ "steps": [read, update] }),
            &json!({ "steps": [lock, read, update] }),
        );

        assert_eq!(paths(&entries), vec!["steps[0]"]);
        assert_eq!(entries[0].base, None);
        assert_eq!(entries[0].head, Some(lock));
    }

    /// A step changed in place is compared field by field; a step whose
    /// kind changed is one replacement.
    #[test]
    fn changed_steps_recurse_or_are_replaced() {
        let read = json!({ "kind": "read", "bind": "read.order" });

        let entries = delta(
            &json!({ "steps": [read, { "kind": "update", "fields": ["status"] }] }),
            &json!({ "steps": [read, { "kind": "compare_and_set", "fields": ["status"] }] }),
        );

        assert_eq!(paths(&entries), vec!["steps[1]"]);
        assert_eq!(entries[0].base.as_ref().unwrap()["kind"], "update");
        assert_eq!(entries[0].head.as_ref().unwrap()["kind"], "compare_and_set");

        let entries = delta(
            &json!({ "steps": [read, { "kind": "update", "fields": ["status"] }] }),
            &json!({ "steps": [read, { "kind": "update", "fields": ["status", "total"] }] }),
        );

        assert_eq!(paths(&entries), vec!["steps[1].fields[1]"]);
    }

    /// Elements carrying ids align by id, and give both indexes where
    /// they moved.
    #[test]
    fn identified_elements_align_by_id() {
        let entries = delta(
            &json!([{ "id": "a", "n": 1 }, { "id": "b", "n": 2 }]),
            &json!([{ "id": "c", "n": 0 }, { "id": "b", "n": 3 }, { "id": "a", "n": 1 }]),
        );

        assert_eq!(paths(&entries), vec!["[1].n", "[0]"]);
        assert_eq!(entries[0].head, Some(json!(3)));

        let entries = delta(
            &json!([{ "id": "a", "n": 1 }, { "id": "b", "n": 2 }]),
            &json!([{ "id": "b", "n": 2 }, { "id": "a", "n": 5 }]),
        );

        assert_eq!(paths(&entries), vec!["[0→1].n"]);
    }
}
