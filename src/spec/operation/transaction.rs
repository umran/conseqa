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

    /// The obligations declared on this transaction's committed
    /// history: serializability and ordering, each keyed by a
    /// value available when the transaction begins.
    #[serde(default)]
    pub requirements: TransactionRequirements,

    pub steps: Vec<TransactionStep>,
}

impl Transaction {
    /// Whether any step of the body is a logical commit guard that may
    /// reject the whole transaction: a compare-and-set, a state
    /// transition, a cursor advance, or a fence. Such a transaction
    /// must carry a `rejected` block at its execution site; one without
    /// any must not.
    pub fn rejects(&self) -> bool {
        self.steps.iter().any(TransactionStep::rejects)
    }
}

/// The requirements of one transaction (§7 of the DSL v4 revision):
/// serializability and ordering. Both families are obligations
/// over the committed state history of every transaction that may
/// conflict with this one — never over an operation program, and
/// never discharged by runtime topology.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionRequirements {
    #[serde(default)]
    pub serializability: Vec<TransactionSerializabilityRequirement>,

    #[serde(default)]
    pub ordering: Vec<TransactionOrderingRequirement>,
}

impl TransactionRequirements {
    pub fn is_empty(&self) -> bool {
        self.serializability.is_empty() && self.ordering.is_empty()
    }
}

/// `SerializableBy(K)`: executions of this transaction whose evaluated
/// keys are equal — together with every transaction in their conflict
/// closure — commit in a history equivalent to some serial order.
///
/// The key identifies the logical conflict domain the obligation is
/// about. It must be available when the transaction begins: an input,
/// a prior transaction output, or a synchronous result already bound
/// on the reaching path. It may not derive from a `transaction_read`
/// performed inside the same transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionSerializabilityRequirement {
    pub key: ValueRef,
}

/// `OrderedBy(K, P)`: within each ordering domain identified by `key`,
/// committed executions of this transaction take effect in the order
/// of their `position` values.
///
/// Both references must be available at transaction entry, and the
/// position must resolve to a non-optional ordered scalar — `int`,
/// `decimal`, or `timestamp`. `float` is excluded because NaN and
/// implementation-specific comparison make it no total order; `uuid`,
/// `bool`, structured schemas, and lists are not positions. The proof
/// rests on transaction serializability plus a persisted cursor or
/// fence whose incoming value is the position; transport precedence
/// is never a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionOrderingRequirement {
    pub key: ValueRef,
    pub position: ValueRef,
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

    /// An unconditional mutation of the selected instances.
    Update(Update),

    /// An atomic conditional update of one identified instance: the
    /// comparison and the mutation are one storage operation, and a
    /// failed comparison — or a missing instance — rejects the
    /// transaction.
    CompareAndSet(CompareAndSet),

    Insert(Insert),

    /// An atomic insert-or-update of one identified instance, arbitrated
    /// on the object's identity.
    Upsert(Upsert),

    Delete(Delete),
    Lock(Lock),

    /// Applies a state-machine transition: an explicit commit guard
    /// over the subject's state that rejects the transaction when the
    /// current state is not among the transition's `from` states.
    Transition(StateTransition),

    /// A commit guard over an ordered cursor field: the transaction
    /// commits only when the incoming position is admissible under the
    /// cursor rule, and then sets the cursor to it atomically.
    AdvanceCursor(AdvanceCursor),

    /// A commit guard over a fencing-token field: a token older than
    /// the persisted fence rejects the transaction; an equal token
    /// leaves the fence; a newer token advances it atomically.
    Fence(Fence),

    EstablishEffectIntent(EstablishEffectIntent),
    EstablishTransactionOutput(EstablishTransactionOutput),

    /// Stages one outbox message for admission atomically with this
    /// transaction's commit — the one legal execution site of an
    /// `OutboxWriteEffect`.
    WriteOutbox(WriteOutboxEffect),
}

impl TransactionStep {
    /// Every value reference the step evaluates: selector roots,
    /// comparison expectations, mutation and artifact derivations,
    /// transition intent derivations, and the declaration roots of an
    /// inline intent's effect contract, which is evaluated at its
    /// establishment site. The transaction's commit key is not a
    /// step's and is judged separately.
    pub fn roots(&self) -> Vec<&ValueRef> {
        let mut roots = match self {
            Self::Read(read) => read.target.predicate.roots(),

            Self::Update(update) => {
                let mut roots = update.target.predicate.roots();

                roots.extend(update.values.roots());

                roots
            }

            Self::CompareAndSet(cas) => {
                let mut roots = cas.target.predicate.roots();

                roots.extend(cas.values.roots());

                roots
            }

            Self::Insert(insert) => insert.values.roots(),

            Self::Upsert(upsert) => {
                let mut roots = upsert.target.predicate.roots();

                roots.extend(upsert.insert_values.roots());
                roots.extend(upsert.update_values.roots());

                roots
            }

            Self::Delete(delete) => delete.target.predicate.roots(),

            Self::Lock(lock) => lock.target.predicate.roots(),

            Self::Transition(transition) => {
                let mut roots = transition.subject.predicate.roots();

                for intent in transition.effect_intents.values() {
                    roots.extend(intent.values.roots());
                }

                for effect in transition.effects.values() {
                    roots.extend(effect.values.roots());
                }

                roots
            }

            Self::AdvanceCursor(advance) => {
                let mut roots = advance.target.predicate.roots();

                roots.push(&advance.incoming);

                roots
            }

            Self::Fence(fence) => {
                let mut roots = fence.target.predicate.roots();

                roots.push(&fence.token);

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
        };

        roots.extend(self.compare().iter().filter_map(CompareCondition::root));

        roots
    }

    /// Whether the step is a logical commit guard that may reject the
    /// containing transaction. The match is deliberately exhaustive: a
    /// new step kind must decide here whether it can reject, rather
    /// than becoming infallible by joining the enum.
    pub fn rejects(&self) -> bool {
        match self {
            Self::CompareAndSet(_)
            | Self::Transition(_)
            | Self::AdvanceCursor(_)
            | Self::Fence(_) => true,

            // An upsert chooses between inserting and updating; neither
            // branch is a logical refusal.
            Self::Read(_)
            | Self::Update(_)
            | Self::Insert(_)
            | Self::Upsert(_)
            | Self::Delete(_)
            | Self::Lock(_)
            | Self::EstablishEffectIntent(_)
            | Self::EstablishTransactionOutput(_)
            | Self::WriteOutbox(_) => false,
        }
    }

    /// The comparisons a guarded mutation conjoins with its intrinsic
    /// condition: a compare-and-set's own, or the optional guard of a
    /// transition, cursor advance, or fence. Empty for every other
    /// step.
    pub fn compare(&self) -> &[CompareCondition] {
        match self {
            Self::CompareAndSet(cas) => &cas.compare,
            Self::Transition(transition) => &transition.compare,
            Self::AdvanceCursor(advance) => &advance.compare,
            Self::Fence(fence) => &fence.compare,

            Self::Read(_)
            | Self::Update(_)
            | Self::Insert(_)
            | Self::Upsert(_)
            | Self::Delete(_)
            | Self::Lock(_)
            | Self::EstablishEffectIntent(_)
            | Self::EstablishTransactionOutput(_)
            | Self::WriteOutbox(_) => &[],
        }
    }

    /// The selector of the persistent instances the step observes,
    /// mutates, or locks. An insert selects nothing — the instance it
    /// creates is fixed by its derivation — and artifact steps touch
    /// no object.
    pub fn selector(&self) -> Option<&ObjectSelector> {
        match self {
            Self::Read(read) => Some(&read.target),
            Self::Update(update) => Some(&update.target),
            Self::CompareAndSet(cas) => Some(&cas.target),
            Self::Upsert(upsert) => Some(&upsert.target),
            Self::Delete(delete) => Some(&delete.target),
            Self::Lock(lock) => Some(&lock.target),
            Self::Transition(transition) => Some(&transition.subject),
            Self::AdvanceCursor(advance) => Some(&advance.target),
            Self::Fence(fence) => Some(&fence.target),

            Self::Insert(_)
            | Self::EstablishEffectIntent(_)
            | Self::EstablishTransactionOutput(_)
            | Self::WriteOutbox(_) => None,
        }
    }

    /// The object the step touches, if any: its selector's, or an
    /// insert's.
    pub fn object(&self) -> Option<&Id> {
        match self {
            Self::Insert(insert) => Some(&insert.object),
            _ => self.selector().map(|selector| &selector.object),
        }
    }

    /// Whether the step is an atomic guarded mutation: a compare-and-
    /// set, a transition, a cursor advance, a fence, or an upsert's
    /// identity arbitration. Each takes the mutated instance's write
    /// protection atomically with its condition and holds it to
    /// commit.
    pub fn guards(&self) -> bool {
        matches!(
            self,
            Self::CompareAndSet(_)
                | Self::Transition(_)
                | Self::AdvanceCursor(_)
                | Self::Fence(_)
                | Self::Upsert(_)
        )
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

/// An unconditional mutation of the selected persistent objects.
///
/// `update` is not an optimistic-concurrency guard: it never rejects
/// because something an earlier read observed has since changed. Where
/// the transaction relies on its read staying true, the mutation is a
/// [`CompareAndSet`] or a guarded domain primitive instead, or the read
/// is protected by a lock or by serializable isolation. On a versioned
/// object the committed mutation publishes a newer version token
/// intrinsically; `fields` never names the version field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub target: ObjectSelector,
    pub fields: BTreeSet<FieldPath>,

    /// Provenance of the values written.
    pub values: Derivation,
}

/// An atomic conditional update of one identified persistent instance.
///
/// Conceptually `if every comparison holds: mutate, else reject` — one
/// storage operation, with no observable interval between a successful
/// comparison and the acquisition of the mutation's write protection,
/// which is then held to commit:
///
/// ```sql
/// UPDATE account SET balance = ?, version = version + 1
///  WHERE account_id = ? AND version = ?;
/// ```
///
/// The target pins the object's whole identity. The transaction
/// rejects when the instance does not exist or any comparison is
/// false. A comparison whose `expected` value is
/// `transaction_read:<bind>.<the same field>` of an earlier read of the
/// same instance is an *observed-state* comparison: it is what lets the
/// serializability checker credit that read as unable to go stale
/// unnoticed. Any other comparison — an input, a literal, another
/// object's read — is application behaviour, valid but no such
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareAndSet {
    pub target: ObjectSelector,

    /// The conditions conjoined into the guard; at least one, each
    /// field at most once.
    pub compare: Vec<CompareCondition>,

    pub fields: BTreeSet<FieldPath>,

    /// Provenance of the values written.
    pub values: Derivation,
}

/// One equality condition of a guarded mutation: the instance's
/// current `field` equals `expected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareCondition {
    pub field: FieldPath,
    pub expected: SelectorValue,
}

impl CompareCondition {
    /// The value reference the condition compares against, if it is
    /// not a literal.
    pub fn root(&self) -> Option<&ValueRef> {
        match &self.expected {
            SelectorValue::Value(root) => Some(root),
            SelectorValue::Literal(_) => None,
        }
    }

    /// The transaction-read binding whose same field the condition
    /// compares against: `expected` is exactly
    /// `transaction_read:<bind>.<field>`. Whether that read precedes
    /// the comparison and selects the same instance is the analyzer's
    /// to judge.
    pub fn observed_read(&self) -> Option<&Id> {
        match self.root() {
            Some(ValueRef {
                source: super::ValueSource::TransactionRead(bind),
                path,
            }) if *path == self.field => Some(bind),
            _ => None,
        }
    }
}

/// An atomic insert-or-update of one identified instance: absent, it is
/// inserted from `insert_values`; present, `update_fields` are set from
/// `update_values`. The choice and the mutation are atomic with respect
/// to competing operations on the same identity, arbitrated on
/// `DataObject.identity`, which the target pins whole.
///
/// An upsert never rejects, and it protects no earlier read: its only
/// concurrency evidence is its own identity arbitration and mutation.
/// On a versioned object the insert branch establishes the initial
/// version and the update branch publishes a newer one; neither names
/// the version field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upsert {
    pub target: ObjectSelector,

    /// Provenance of the inserted contents.
    pub insert_values: Derivation,

    pub update_fields: BTreeSet<FieldPath>,

    /// Provenance of the values the update branch writes.
    pub update_values: Derivation,
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

    /// Observed-state or application comparisons conjoined with the
    /// transition's intrinsic `from` guard, in the same atomic
    /// conditional update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compare: Vec<CompareCondition>,

    /// The intents this application establishes, keyed by the
    /// state-machine transition side-effect ID.
    ///
    /// The keys must exactly match the transition's declared side
    /// effects; a transition without side effects uses an empty map.
    /// The derivations are evaluated in the enclosing transaction
    /// context at this step, so they may reference preceding
    /// transaction reads.
    pub effect_intents: BTreeMap<Id, TransitionEffectIntent>,

    /// The message derivations of the transition's declared outbox
    /// effects, keyed by the transition's effect ID.
    ///
    /// The keys must exactly match the transition's declared
    /// `effects`. Each message is admitted to its outbox atomically
    /// with the transaction iff this transition applies; a rejected
    /// transition admits none of them. The derivations are evaluated
    /// in the enclosing transaction context at this step.
    #[serde(default)]
    pub effects: BTreeMap<Id, TransitionEffectApplication>,
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

/// One transition-scoped outbox effect's application facts: the
/// provenance of the complete logical message admitted when the
/// transition applies. Nothing is bound — an admission has no
/// synchronous result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionEffectApplication {
    /// Provenance of the admitted message's logical contents.
    pub values: Derivation,
}

/// The ordered-cursor commit guard and the only ordinary update of a
/// cursor field.
///
/// For stored position `S` and incoming position `P`, the transaction
/// commits only when the rule admits `P` after `S`, and then sets the
/// cursor to `P` atomically with the commit. An inadmissible position
/// — stale, duplicate, or (under `successor`) a gap — rejects the
/// transaction. Two successful advances of one cursor domain are
/// therefore commit-ordered by their accepted positions, which is what
/// an `OrderedBy(K, P)` proof consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvanceCursor {
    pub target: ObjectSelector,

    /// The cursor field on the target object: a non-optional `int`
    /// under `successor`, or a non-optional `int`, `decimal`, or
    /// `timestamp` under `monotonic_after`.
    pub field: FieldPath,

    /// The incoming position, of the cursor field's type.
    pub incoming: ValueRef,

    pub rule: CursorAdvanceRule,

    /// Comparisons conjoined with the cursor rule in the same atomic
    /// conditional update. They change nothing about the ordering the
    /// cursor establishes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compare: Vec<CompareCondition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorAdvanceRule {
    /// Admits `P` iff `P = S + 1`: gap-free progression. A stale or
    /// duplicate position and a gap both reject.
    Successor,

    /// Admits `P` iff `P > S`: monotonic progression that permits
    /// gaps — high-water marks, snapshot versions, log positions,
    /// superseding updates. Not sufficient where every predecessor
    /// must be applied.
    MonotonicAfter,
}

impl std::fmt::Display for CursorAdvanceRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Successor => "successor",
            Self::MonotonicAfter => "monotonic_after",
        })
    }
}

/// The fencing commit guard: for persisted fence `F` and incoming
/// token `T`, `T < F` rejects the transaction, `T = F` leaves the
/// authority valid, and `T > F` advances the fence to `T` atomically
/// with the commit.
///
/// A fence asserts that state protected by the transaction cannot be
/// mutated by an older authority generation after a newer generation
/// has been accepted. It does not claim the stale worker has
/// terminated, and equal tokens establish no relative order — which
/// is why fencing is not by itself a serializability proof, only an
/// ordering route on top of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub target: ObjectSelector,

    /// The fence field on the target object: a non-optional `int`,
    /// `decimal`, or `timestamp`.
    pub field: FieldPath,

    /// The incoming authority token, of the fence field's type.
    pub token: ValueRef,

    /// Comparisons conjoined with the fencing condition in the same
    /// atomic conditional update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compare: Vec<CompareCondition>,
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
