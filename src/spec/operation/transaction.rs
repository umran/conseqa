use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::value::{MapAccessDeserializer, StrDeserializer};
use serde::de::{self, DeserializeSeed, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::spec::operation::value::opens_with_value_source_kind;
use crate::spec::{FieldPath, Id};

use super::{Derivation, Effect, IdempotencyGuarantee, OutboxWriteEffect, ValueRef};

/// One atomic transaction, declared and executed at the program step
/// that carries it.
///
/// `id` is the stable logical identity of this inline declaration — the
/// durable keyed-commit identity is conceptually
/// `Commit(operation, id, key)` — used for keyed commit recovery,
/// conformance, proof evidence, and diagnostics. It is not a reference
/// to another declaration, and it must be unique within the operation:
/// one inline transaction declaration is one transaction occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transaction {
    /// Stable identity of this inline transaction.
    pub id: Id,

    /// None is permitted when the transaction performs no application
    /// DataObject access and only produces or consumes framework
    /// transaction artifacts.
    pub data_model: Option<Id>,

    pub isolation: TransactionIsolation,

    /// Explicit durable keyed commit deduplication provided by the
    /// execution environment.
    ///
    /// This is independent of any transaction-output or effect-intent
    /// binding. `Unspecified` and `NotDeduplicated` leave the analyzer
    /// free to prove natural replayability from the body.
    pub idempotency: IdempotencyGuarantee,

    pub steps: Vec<TransactionStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionIsolation {
    Unspecified,
    ReadCommitted,
    Snapshot,
    Serializable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionStep {
    Read(Read),
    Write(Write),
    Insert(Insert),
    Delete(Delete),
    Lock(Lock),

    Transition(StateTransition),
    EstablishEffectIntent(EstablishEffectIntent),
    EstablishTransactionOutput(EstablishTransactionOutput),

    /// Stages one outbox message for admission atomically with this
    /// transaction's commit — the one legal execution site of an
    /// `OutboxWriteEffect`.
    WriteOutbox(WriteOutboxEffect),
}

impl TransactionStep {
    /// Every value reference the step evaluates: selector roots,
    /// mutation and artifact derivations, transition intent
    /// derivations, and the declaration roots of an inline intent's
    /// effect contract, which is evaluated at its establishment site.
    /// The transaction's commit key is not a step's and is judged
    /// separately.
    pub fn roots(&self) -> Vec<&ValueRef> {
        match self {
            Self::Read(read) => read.target.predicate.roots(),

            Self::Write(write) => {
                let mut roots = write.target.predicate.roots();

                roots.extend(write.values.roots());

                roots
            }

            Self::Insert(insert) => insert.values.roots(),

            Self::Delete(delete) => delete.target.predicate.roots(),

            Self::Lock(lock) => lock.target.predicate.roots(),

            Self::Transition(transition) => {
                let mut roots = transition.subject.predicate.roots();

                for intent in transition.effect_intents.values() {
                    roots.extend(intent.values.roots());
                }

                roots
            }

            Self::EstablishEffectIntent(establish) => {
                let mut roots = establish.effect.roots();

                roots.extend(establish.values.roots());

                roots
            }

            Self::EstablishTransactionOutput(establish) => establish.values.roots(),

            Self::WriteOutbox(write) => {
                let mut roots = write.values.roots();

                for propagation in &write.effect.idempotency_key_propagation {
                    roots.extend(propagation.source.components.iter());
                    roots.extend(propagation.target.components.iter());
                }

                roots
            }
        }
    }
}

/// Observes selected fields of persistent objects, binding the
/// observation for later steps of the same transaction.
///
/// The binding exists only after the read and only inside this
/// transaction execution; it never becomes a transaction artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Read {
    /// Transaction-local binding of this observation.
    ///
    /// Later steps in the same transaction may reference the observed
    /// values through `ValueSource::TransactionRead`.
    pub bind: Id,

    pub target: ObjectSelector,
    pub fields: FieldSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    pub target: ObjectSelector,
    pub fields: BTreeSet<FieldPath>,

    /// Provenance of the values written.
    pub values: Derivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Insert {
    pub object: Id,

    /// Provenance of the inserted contents.
    ///
    /// An insert never redeclares object identity: `DataObject.identity`
    /// is already the complete logical identity of every instance.
    pub values: Derivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delete {
    pub target: ObjectSelector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub target: ObjectSelector,
    pub mode: LockMode,
    pub order: LockOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "terms", rename_all = "snake_case")]
pub enum LockOrder {
    Unspecified,
    By(Vec<OrderingTerm>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderingTerm {
    pub field: FieldPath,
    pub direction: OrderingDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderingDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "fields", rename_all = "snake_case")]
pub enum FieldSelection {
    All,
    Only(BTreeSet<FieldPath>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectSelector {
    pub object: Id,
    pub predicate: SelectorPredicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectorPredicate {
    All,

    Eq {
        field: FieldPath,
        value: SelectorValue,
    },

    And {
        predicates: Vec<SelectorPredicate>,
    },
}

impl SelectorPredicate {
    /// Every value reference the predicate constrains against.
    /// Literals are constants and contribute nothing.
    pub fn roots(&self) -> Vec<&ValueRef> {
        match self {
            Self::All => Vec::new(),

            Self::Eq { value, .. } => match value {
                SelectorValue::Value(root) => vec![root],
                SelectorValue::Literal(_) => Vec::new(),
            },

            Self::And { predicates } => predicates
                .iter()
                .flat_map(SelectorPredicate::roots)
                .collect(),
        }
    }
}

/// What a selector compares a field against.
///
/// The canonical form is the tagged map. The two alternatives are
/// structurally disjoint, so each may also be written as itself: a
/// reference is a map, a literal is a plain scalar.
///
/// ```yaml
/// value:
///   source: input:input.transfer_stock.request
///   path: sku
///
/// value: pending
/// ```
///
/// Inferring *this* discriminant is safe where inferring a
/// `ValueSource`'s kind is not: nothing has to be resolved to tell a
/// map from a scalar, whereas the five value sources are all ids and
/// differ only in which namespace they name. §19 relies on a selector
/// exposing its literals and references structurally, and the
/// shorthand keeps that distinction visible rather than defaulting
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SelectorValue {
    Value(ValueRef),
    Literal(Literal),
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    rename = "SelectorValue"
)]
enum SelectorValueLong {
    Value(ValueRef),
    Literal(Literal),
}

impl From<SelectorValueLong> for SelectorValue {
    fn from(long: SelectorValueLong) -> Self {
        match long {
            SelectorValueLong::Value(value) => SelectorValue::Value(value),
            SelectorValueLong::Literal(literal) => SelectorValue::Literal(literal),
        }
    }
}

impl<'de> Deserialize<'de> for SelectorValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SelectorValueVisitor)
    }
}

struct SelectorValueVisitor;

impl<'de> Visitor<'de> for SelectorValueVisitor {
    type Value = SelectorValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a selector value: a value reference with `source` and `path`, \
             a literal scalar such as `pending`, `true`, or `3`, \
             or a map with `kind` and `value`",
        )
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let Some(key) = map.next_key::<String>()? else {
            return Err(de::Error::custom(
                "an empty map is neither a value reference nor a literal",
            ));
        };

        // The key that opens the map says which form this is, and is
        // replayed so the derived reader still sees a whole map.
        match key.as_str() {
            "source" | "path" => {
                ValueRef::deserialize(MapAccessDeserializer::new(Replayed::new(key, map)))
                    .map(SelectorValue::Value)
            }
            "kind" | "value" => {
                SelectorValueLong::deserialize(MapAccessDeserializer::new(Replayed::new(key, map)))
                    .map(SelectorValue::from)
            }
            other => Err(de::Error::custom(format!(
                "unknown field `{other}`, expected `source` and `path` for a value reference, \
                 or `kind` and `value`"
            ))),
        }
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        // A reference that lost its path would otherwise read as the
        // string it spells, quietly turning a provenance-bearing
        // comparison into a comparison with a constant.
        if opens_with_value_source_kind(text) {
            return Err(E::custom(format!(
                "`{text}` reads as a string literal, but names a value source kind; \
                 a value reference is a map with `source` and `path`"
            )));
        }

        Ok(SelectorValue::Literal(Literal::String(text.to_string())))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(SelectorValue::Literal(Literal::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(SelectorValue::Literal(Literal::Int(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i64::try_from(value)
            .map(|value| SelectorValue::Literal(Literal::Int(value)))
            .map_err(|_| E::custom(format!("`{value}` does not fit an int literal")))
    }
}

/// A `MapAccess` that replays one already-read key before the rest of
/// the map, so a peeked key can still be handed to a derived reader.
struct Replayed<A> {
    key: Option<String>,
    rest: A,
}

impl<A> Replayed<A> {
    fn new(key: String, rest: A) -> Self {
        Replayed {
            key: Some(key),
            rest,
        }
    }
}

impl<'de, A> MapAccess<'de> for Replayed<A>
where
    A: MapAccess<'de>,
{
    type Error = A::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        match self.key.take() {
            Some(key) => seed
                .deserialize(StrDeserializer::<Self::Error>::new(&key))
                .map(Some),
            None => self.rest.next_key_seed(seed),
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        self.rest.next_value_seed(seed)
    }

    fn size_hint(&self) -> Option<usize> {
        self.rest
            .size_hint()
            .map(|rest| rest + usize::from(self.key.is_some()))
    }
}

/// A constant value.
///
/// The canonical form is the tagged map; a plain scalar carries the
/// same thing, typed as YAML types it. A string that YAML would read
/// as a bool or an int is written quoted, as it is anywhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Literal {
    String(String),
    Bool(bool),
    Int(i64),
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    rename = "Literal"
)]
enum LiteralLong {
    String(String),
    Bool(bool),
    Int(i64),
}

impl From<LiteralLong> for Literal {
    fn from(long: LiteralLong) -> Self {
        match long {
            LiteralLong::String(value) => Literal::String(value),
            LiteralLong::Bool(value) => Literal::Bool(value),
            LiteralLong::Int(value) => Literal::Int(value),
        }
    }
}

impl<'de> Deserialize<'de> for Literal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(LiteralVisitor)
    }
}

struct LiteralVisitor;

impl<'de> Visitor<'de> for LiteralVisitor {
    type Value = Literal;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a literal: a scalar such as `pending`, `true`, or `3`, \
             or a map with `kind` and `value`",
        )
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Literal::String(text.to_string()))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Literal::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Literal::Int(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i64::try_from(value)
            .map(Literal::Int)
            .map_err(|_| E::custom(format!("`{value}` does not fit an int literal")))
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        LiteralLong::deserialize(MapAccessDeserializer::new(map)).map(Literal::from)
    }
}

/// Applies a state-machine transition, supplying for each of the
/// transition's declared side effects the concrete instance derivation
/// and an operation-local intent binding.
///
/// A successful transaction atomically applies the state transition,
/// constructs each side-effect instance, establishes each bound intent
/// artifact, and commits state and artifacts together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateTransition {
    pub machine: Id,
    pub transition: Id,

    /// Selects the concrete persistent machine instance.
    pub subject: ObjectSelector,

    /// The intents this application establishes, keyed by the
    /// state-machine transition side-effect ID.
    ///
    /// The keys must exactly match the transition's declared side
    /// effects; a transition without side effects uses an empty map.
    /// The derivations are evaluated in the enclosing transaction
    /// context at this step, so they may reference preceding
    /// transaction reads.
    pub effect_intents: BTreeMap<Id, TransitionEffectIntent>,
}

/// One transition side effect's application facts: the concrete
/// instance derivation and the operation-local binding under which the
/// intent artifact is established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionEffectIntent {
    /// Operation-local binding of the established intent artifact.
    pub bind: Id,

    /// Provenance of the intent's logical contents.
    pub values: Derivation,
}

/// Declares an effect contract, constructs one concrete logical effect
/// instance from `values`, and atomically establishes that captured
/// instance as the `EffectIntent` artifact named by `bind`.
///
/// `effect_id` identifies the captured logical effect site itself; the
/// intent binding is not the effect declaration. The contract's own
/// value references are evaluated in the enclosing transaction context
/// at this step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstablishEffectIntent {
    /// Binding of the established `EffectIntent` artifact.
    pub bind: Id,

    /// Stable identity of the captured inline effect occurrence.
    pub effect_id: Id,

    /// The logical effect contract declared at this site.
    pub effect: Effect,

    /// Provenance of the intent's logical contents.
    pub values: Derivation,
}

/// Declares an `OutboxWriteEffect` contract, constructs one concrete
/// logical message instance from `values`, and stages its admission to
/// the destination outbox inside the current transaction. The message
/// becomes durable if and only if the containing transaction commits.
///
/// This step is an effect execution site, not a persistent-object
/// insertion: `effect_id` has the same stable execution-site role as
/// other inline effect IDs — value lineage, idempotency-key
/// propagation, diagnostics, proof evidence, visualization. The
/// derivation is evaluated in the transaction context at this step, so
/// it may reference transaction-local values valid at that point. The
/// step binds nothing: an outbox write has no synchronous result, and
/// its payload is not implicitly available to later control merely
/// because the write succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteOutboxEffect {
    /// Stable identity of this inline effect occurrence.
    pub effect_id: Id,

    /// The transactional outbox-write contract declared at this site.
    pub effect: OutboxWriteEffect,

    /// Provenance of the complete logical message instance.
    pub values: Derivation,
}

/// Exports a typed value from the transaction into the enclosing
/// operation's control.
///
/// The binder declares in one place the artifact's binding, schema,
/// producer transaction and step, and derivation: the transaction
/// constructs a value shaped by `schema`, declares its provenance
/// through `values`, establishes the artifact atomically with its
/// commit, and makes `bind` available to the operation control that
/// follows a successful execution or a commit recovery. It implies no
/// response, no success or failure, no effect execution, no
/// idempotency, and no storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstablishTransactionOutput {
    /// Binding of the exported artifact.
    pub bind: Id,

    /// Shape of the exported logical value.
    pub schema: Id,

    /// Provenance of the output's logical contents.
    pub values: Derivation,
}
