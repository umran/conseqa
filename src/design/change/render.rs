//! The diff for people (§15.2 of the spec change analysis revision):
//! what `conseqa diff` prints, and the summary `diff_spec` puts in
//! front of the document.
//!
//! Presentation only. Everything here is read from the diff document,
//! so the text never says what the document does not, and a reader who
//! wants more takes the document itself.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::analyzer::basis::{BasisFact, DeclarationKey, Layer, ObligationKey};
use crate::analyzer::report::Status;

use super::delta::DeltaEntry;
use super::diff::{
    Change, ClassCounts, DeclarationChange, DiffDocument, ObligationClass, ObligationDiff,
    ObligationEntry, RelianceDelta, SideRecord,
};
use super::partition::DeclarationLayer;

/// The classes in the order entries are printed: what fails a review
/// first, what only informs it last.
const CLASS_ORDER: [ObligationClass; 6] = [
    ObligationClass::Regressed,
    ObligationClass::Rebased,
    ObligationClass::Improved,
    ObligationClass::Unproven,
    ObligationClass::Added,
    ObligationClass::Held,
];

/// The diff in full, as `conseqa diff` prints it.
pub fn render(document: &DiffDocument) -> String {
    let mut out = String::new();

    header(&mut out, document);
    out.push('\n');
    declarations(&mut out, document, true);
    out.push('\n');
    obligations(&mut out, document, true);

    if let Some(reliance) = &document.reliance {
        out.push('\n');
        reliance_section(&mut out, reliance);
    }

    out
}

/// The diff in brief: the counts, and the entries a reviewer must not
/// miss (regressed, removed, unexplained). What `diff_spec` puts in
/// front of the document.
pub fn summary(document: &DiffDocument) -> String {
    let mut out = String::new();

    header(&mut out, document);
    declarations(&mut out, document, false);
    obligations(&mut out, document, false);

    if let Some(reliance) = &document.reliance {
        let _ = writeln!(
            out,
            "reliance       {} introduced, {} withdrawn",
            reliance.introduced.len(),
            reliance.withdrawn.len()
        );
    }

    out
}

fn header(out: &mut String, document: &DiffDocument) {
    let _ = writeln!(
        out,
        "base  {}    head  {}",
        side(&document.base),
        side(&document.head)
    );
}

fn side(record: &SideRecord) -> String {
    let source = &record.source;

    let mut text = if source.working_tree {
        "working tree".to_string()
    } else {
        let reference = source.reference.as_deref().unwrap_or("commit");
        let commit = source
            .commit
            .as_deref()
            .map(|commit| format!(" ({})", &commit[..commit.len().min(7)]))
            .unwrap_or_default();

        if source.merge_base {
            format!("merge base of {reference}{commit}")
        } else {
            format!("{reference}{commit}")
        }
    };

    if record.empty {
        text.push_str(", empty");
    }

    text
}

fn declarations(out: &mut String, document: &DiffDocument, detail: bool) {
    let changes = &document.declarations.changes;
    let authoring = &document.declarations.authoring;

    let by_change = |wanted: Change| {
        changes
            .iter()
            .filter(|change| change.change == wanted)
            .count()
    };

    let by_layer = |wanted: DeclarationLayer| {
        changes
            .iter()
            .filter(|change| change.layer == wanted)
            .count()
    };

    let counts = joined(&[
        (by_change(Change::Modified), "modified"),
        (by_change(Change::Added), "added"),
        (by_change(Change::Removed), "removed"),
    ]);

    let layers = [
        (DeclarationLayer::L0, "l0"),
        (DeclarationLayer::L1, "l1"),
        (DeclarationLayer::Intent, "intent"),
    ]
    .into_iter()
    .filter_map(|(layer, name)| {
        let count = by_layer(layer);
        (count > 0).then(|| format!("{name} {count}"))
    })
    .collect::<Vec<_>>()
    .join(", ");

    let mut line = if changes.is_empty() {
        "no semantic change".to_string()
    } else {
        format!("{counts}    {layers}")
    };

    if !authoring.is_empty() {
        let _ = write!(line, "    authoring {}", authoring.len());
    }

    let _ = writeln!(out, "declarations   {line}");

    if !detail {
        return;
    }

    for change in changes {
        declaration_change(out, change);
    }

    for change in authoring {
        let _ = writeln!(
            out,
            "  {} {} (authoring)",
            marker(change.change),
            change.item
        );
    }
}

fn declaration_change(out: &mut String, change: &DeclarationChange) {
    let _ = writeln!(
        out,
        "  {} {}",
        marker(change.change),
        declaration(&change.declaration)
    );

    let width = change
        .delta
        .iter()
        .map(|entry| entry.path.chars().count())
        .max()
        .unwrap_or(0);

    for entry in &change.delta {
        let _ = writeln!(out, "      {:width$}  {}", entry.path, delta_entry(entry));
    }
}

fn delta_entry(entry: &DeltaEntry) -> String {
    match (&entry.base, &entry.head) {
        (Some(base), Some(head)) => format!("{} -> {}", value(base), value(head)),
        (None, Some(head)) => format!("+ {}", value(head)),
        (Some(base), None) => format!("- {}", value(base)),
        (None, None) => String::new(),
    }
}

/// A value in a delta, briefly: a scalar as itself, a step or any other
/// tagged map by its kind, and anything else as compact JSON while it
/// is short.
fn value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),

        Value::Object(fields) => match fields.get("kind").and_then(Value::as_str) {
            Some(kind) => kind.to_string(),
            None => short(value, "{…}"),
        },

        Value::Array(items) => short(value, &format!("[{} items]", items.len())),
    }
}

fn short(value: &Value, otherwise: &str) -> String {
    let text = value.to_string();

    if text.chars().count() <= 60 {
        text
    } else {
        otherwise.to_string()
    }
}

fn obligations(out: &mut String, document: &DiffDocument, detail: bool) {
    let (counts, entries, removed, unexplained) = match &document.obligations {
        ObligationDiff::Available {
            counts,
            entries,
            removed,
            unexplained,
        } => (counts, entries, removed, unexplained),

        ObligationDiff::Unavailable {
            side,
            reason,
            other_side,
        } => {
            let _ = writeln!(out, "obligations    unavailable: {side}: {reason}");

            if let Some(other) = other_side {
                let _ = writeln!(
                    out,
                    "               the other side has {} proven, {} unknown, {} disproven",
                    other.proven, other.unknown, other.disproven
                );
            }

            return;
        }
    };

    let _ = writeln!(out, "obligations    {}", class_counts(counts));

    let changes: BTreeMap<&DeclarationKey, Change> = document
        .declarations
        .changes
        .iter()
        .map(|change| (&change.declaration, change.change))
        .collect();

    let changed = |key: &DeclarationKey| {
        let marker = changes.get(key).copied().map(marker).unwrap_or(' ');

        format!("{marker} {}", declaration(key))
    };

    for class in CLASS_ORDER {
        for entry in entries.iter().filter(|entry| entry.class == class) {
            if detail || class == ObligationClass::Regressed {
                obligation_entry(out, entry, detail, &changed);
            }
        }
    }

    if !removed.is_empty() {
        if detail {
            out.push('\n');
        }

        let _ = writeln!(
            out,
            "removed        {}",
            counted(counts.removed, "obligation")
        );

        for group in removed {
            let _ = writeln!(out, "  {}", changed(&group.declaration));

            for key in &group.obligations {
                let status = entries
                    .iter()
                    .find(|entry| &entry.key == key)
                    .and_then(|entry| entry.base_status)
                    .map(status_name)
                    .unwrap_or_default();

                let _ = writeln!(out, "      {key}    {status}");
            }
        }
    }

    if !unexplained.is_empty() {
        if detail {
            out.push('\n');
        }

        let _ = writeln!(
            out,
            "unexplained    {}: a changed verdict no changed declaration explains, \
             an analyzer defect",
            counted(unexplained.len(), "entry")
        );

        for entry in unexplained {
            let _ = writeln!(out, "  ! {}  {}", class_name(entry.class), entry.key);
        }
    }
}

fn class_counts(counts: &ClassCounts) -> String {
    let matched = joined(&[
        (counts.held, "held"),
        (counts.rebased, "rebased"),
        (counts.regressed, "regressed"),
        (counts.improved, "improved"),
        (counts.unproven, "unproven"),
    ]);

    let matched = if matched.is_empty() {
        format!("{} matched", counts.matched())
    } else {
        format!("{} matched: {matched}", counts.matched())
    };

    format!(
        "{matched}; {} added; {} removed",
        counts.added, counts.removed
    )
}

fn obligation_entry(
    out: &mut String,
    entry: &ObligationEntry,
    detail: bool,
    changed: &dyn Fn(&DeclarationKey) -> String,
) {
    let mut line = format!(
        "  {} {:<10} {}",
        class_marker(entry.class),
        class_name(entry.class),
        entry.key
    );

    if entry.class == ObligationClass::Added
        && let Some(status) = entry.head_status
    {
        let _ = write!(line, "    {}", status_name(status));
    }

    if entry.restated == Some(true) {
        line.push_str("    restated");
    }

    let _ = writeln!(out, "{line}");

    if !detail {
        return;
    }

    if matches!(
        entry.class,
        ObligationClass::Regressed | ObligationClass::Improved
    ) && let (Some(base), Some(head)) = (entry.base_status, entry.head_status)
    {
        let _ = writeln!(out, "      {} -> {}", status_name(base), status_name(head));
    }

    if let Some(scope) = &entry.scope {
        let _ = write!(
            out,
            "      scope  {} -> {}",
            scope_name(scope.base),
            scope_name(scope.head)
        );

        if !scope.through.is_empty() {
            let _ = write!(out, ", through {}", keys(&scope.through));
        }

        out.push('\n');
    }

    if entry.blocked_on_changed == Some(true) {
        let _ = writeln!(out, "      blocked on different facts");
    }

    facts(out, "withdrawn ", &entry.withdrawn);
    facts(out, "introduced", &entry.introduced);

    let label = match entry.class {
        ObligationClass::Held => "touched     ",
        ObligationClass::Added
        | ObligationClass::Removed
        | ObligationClass::Regressed
        | ObligationClass::Improved
        | ObligationClass::Rebased
        | ObligationClass::Unproven => "explained by",
    };

    for declaration_key in &entry.explained_by {
        let _ = writeln!(out, "      {label}  {}", changed(declaration_key));
    }

    for key in &entry.explained_through {
        let _ = writeln!(out, "      explained through  {key}");
    }
}

fn facts(out: &mut String, label: &str, facts: &[BasisFact]) {
    for fact in facts {
        let _ = writeln!(
            out,
            "      {label}  {}{}",
            fact.fact,
            layer_note(fact.layer)
        );
    }
}

fn reliance_section(out: &mut String, reliance: &RelianceDelta) {
    if reliance.introduced.is_empty() && reliance.withdrawn.is_empty() {
        let _ = writeln!(out, "reliance       unchanged");

        return;
    }

    let _ = writeln!(out, "reliance");

    for introduced in &reliance.introduced {
        let _ = writeln!(
            out,
            "  + {}{}    {}",
            introduced.fact,
            layer_note(introduced.layer),
            counted(introduced.relied_on_by, "obligation")
        );
    }

    for withdrawn in &reliance.withdrawn {
        let _ = writeln!(
            out,
            "  - {}{}    no longer relied on",
            withdrawn.fact,
            layer_note(withdrawn.layer)
        );
    }
}

/// A declaration for a reader: its kind and its ids, or, for a
/// requirement, which has no textual key, its family, key, and
/// subject.
fn declaration(declaration: &DeclarationKey) -> String {
    match (declaration, declaration.ids()) {
        (DeclarationKey::Requirement(identity), _) => format!("requirement  {identity}"),

        (_, Some(ids)) => format!(
            "{}  {}",
            declaration.kind(),
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" / ")
        ),

        (_, None) => declaration.kind().to_string(),
    }
}

fn keys(keys: &[ObligationKey]) -> String {
    keys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn layer_note(layer: Layer) -> &'static str {
    match layer {
        Layer::L0 => "",
        Layer::L1 => "   (l1)",
    }
}

fn marker(change: Change) -> char {
    match change {
        Change::Added => '+',
        Change::Removed => '-',
        Change::Modified => '~',
    }
}

fn class_marker(class: ObligationClass) -> char {
    match class {
        ObligationClass::Regressed => '!',
        ObligationClass::Rebased => '~',
        ObligationClass::Improved => '^',
        ObligationClass::Unproven => '?',
        ObligationClass::Added => '+',
        ObligationClass::Removed => '-',
        ObligationClass::Held => '=',
    }
}

fn class_name(class: ObligationClass) -> &'static str {
    match class {
        ObligationClass::Added => "added",
        ObligationClass::Removed => "removed",
        ObligationClass::Regressed => "regressed",
        ObligationClass::Improved => "improved",
        ObligationClass::Held => "held",
        ObligationClass::Rebased => "rebased",
        ObligationClass::Unproven => "unproven",
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Proven => "proven",
        Status::Unknown => "unknown",
        Status::Disproven => "disproven",
    }
}

fn scope_name(scope: crate::analyzer::verification::ProofScope) -> &'static str {
    match scope {
        crate::analyzer::verification::ProofScope::L0Only => "l0_only",
        crate::analyzer::verification::ProofScope::RuntimeDependent => "runtime_dependent",
    }
}

/// `n noun` or `n nouns`.
fn counted(count: usize, noun: &str) -> String {
    match (count, noun) {
        (1, _) => format!("1 {noun}"),
        (_, "entry") => format!("{count} entries"),
        _ => format!("{count} {noun}s"),
    }
}

/// The non-zero counts, as `3 modified, 1 added`.
fn joined(counts: &[(usize, &str)]) -> String {
    counts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{count} {name}"))
        .collect::<Vec<_>>()
        .join(", ")
}
