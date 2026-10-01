//! Transactions in words.
//!
//! A System One decider is asked about a prompt, never about Conseqa:
//! what a transaction does reaches it as a sentence a prompt could be
//! matched against, not as DSL. Locks, version guards and established
//! artifacts are how a transaction protects or reports its work, not
//! work a prompt would describe, so they are left out.

use crate::spec::{
    FieldPath, ObjectSelector, SelectorPredicate, SelectorValue, Transaction, TransactionStep,
    ValueRef, ValueSource,
};

/// What the transaction reads and changes, or `None` when it changes
/// nothing — it then has no committed history to constrain.
pub(super) fn summarize(transaction: &Transaction) -> Option<String> {
    let mut phrases = Vec::new();
    let mut mutates = false;

    for step in &transaction.steps {
        let phrase = match step {
            TransactionStep::Read(read) => Some(format!("reads {}", selected(&read.target))),

            TransactionStep::Write(write) => {
                mutates = true;

                Some(format!("updates {}", selected(&write.target)))
            }

            TransactionStep::Insert(insert) => {
                mutates = true;

                Some(format!("inserts a new `{}`", insert.object))
            }

            TransactionStep::Delete(delete) => {
                mutates = true;

                Some(format!("deletes {}", selected(&delete.target)))
            }

            TransactionStep::Transition(transition) => {
                mutates = true;

                Some(format!(
                    "applies transition `{}` to {}",
                    transition.transition,
                    selected(&transition.subject)
                ))
            }

            TransactionStep::AdvanceCursor(advance) => {
                mutates = true;

                Some(format!(
                    "advances the `{}` position of {} to `{}`",
                    advance.field,
                    selected(&advance.target),
                    label(&advance.incoming)
                ))
            }

            TransactionStep::Fence(fence) => {
                mutates = true;

                Some(format!(
                    "fences {} with token `{}`",
                    selected(&fence.target),
                    label(&fence.token)
                ))
            }

            TransactionStep::WriteOutbox(write) => {
                mutates = true;

                Some(format!(
                    "records a `{}` message in outbox `{}`",
                    write.effect.schema, write.effect.outbox
                ))
            }

            TransactionStep::Lock(_)
            | TransactionStep::ValidateVersion(_)
            | TransactionStep::BumpVersion(_)
            | TransactionStep::EstablishEffectIntent(_)
            | TransactionStep::EstablishTransactionOutput(_) => None,
        };

        phrases.extend(phrase);
    }

    mutates.then(|| phrases.join(", then "))
}

pub(super) fn is_input(value: &ValueRef) -> bool {
    matches!(value.source, ValueSource::Input(_))
}

pub(super) fn label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

/// The `field = value` terms of a selector's predicate.
pub(super) fn pins(predicate: &SelectorPredicate) -> Vec<(&FieldPath, &SelectorValue)> {
    match predicate {
        SelectorPredicate::All => Vec::new(),
        SelectorPredicate::Eq { field, value } => vec![(field, value)],
        SelectorPredicate::And { predicates } => predicates.iter().flat_map(pins).collect(),
    }
}

pub(super) fn selected(selector: &ObjectSelector) -> String {
    let fields: Vec<String> = pins(&selector.predicate)
        .into_iter()
        .map(|(field, _)| format!("`{field}`"))
        .collect();

    if fields.is_empty() {
        format!("every `{}`", selector.object)
    } else {
        format!(
            "the `{}` selected by {}",
            selector.object,
            fields.join(" and ")
        )
    }
}
