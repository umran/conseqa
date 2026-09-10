//! L1 — runtime topology and realization semantics.
//!
//! L0 (everything else in `spec`) defines the abstract application
//! machine: logical structure, invocation relationships, programs,
//! transactions, effects, behavioral guarantees, and correctness
//! requirements. L1 describes selected facts about how that machine is
//! realized: message transport ordering, subscription delivery and
//! dispatch, request routing, execution-resource topology, execution
//! concurrency, and physical storage partitioning.
//!
//! The governing rule is:
//!
//! > L0 says what application machine exists and what properties it
//! > requires. L1 says how invocations and persistent data are
//! > arranged in a particular runtime realization of that machine.
//!
//! L1 is optional. An L0-only model is complete and analyzable; it
//! simply has fewer facts from which to discharge its obligations. The
//! analyzer may combine L0 and L1 facts freely, but any proof that
//! consumed an L1 fact is recorded as runtime-dependent, because it
//! holds only of that realization.
//!
//! Two boundaries are deliberate and load-bearing:
//!
//! - A semantic layer is not a correctness layer. `SerializedBy(K)`,
//!   transaction isolation, and explicit locks are all L0 despite
//!   being implemented by infrastructure; topic transport ordering and
//!   member concurrency are L1 despite being invisible to a caller.
//!   Correctness relevance does not determine layer.
//! - A routing key is a *semantic* value derived from an L0
//!   invocation. It names a routing domain, never an execution-pool
//!   member, worker, process, shard, host, or storage partition.
//!   [`MemberAssignment`] — and only it — maps routing domains onto
//!   the runtime topology, so a domain keeps its identity across
//!   rebalances.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::{FieldPath, Id};

/// The declared runtime realization of the L0 model.
///
/// Every collection is independently optional: a model may describe
/// execution topology without storage layout, transport ordering
/// without routing, and so on. Absence is epistemic — no fact — never
/// an assertion that the realization lacks the property.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeModel {
    /// Transport facts for L0 topics, keyed by topic ID.
    #[serde(default)]
    pub topics: BTreeMap<Id, TopicRuntime>,

    /// Delivery and dispatch facts for L0 subscription inputs, keyed
    /// by operation ID and then by input ID.
    #[serde(default)]
    pub subscriptions: BTreeMap<Id, BTreeMap<Id, SubscriptionRuntime>>,

    /// The execution-resource populations invocations are assigned to.
    #[serde(default)]
    pub execution_pools: BTreeMap<Id, ExecutionPool>,

    /// Request routing, one router per L0 request boundary at most.
    #[serde(default)]
    pub routers: BTreeMap<Id, Router>,

    /// Physical partitioning of L0 data objects.
    #[serde(default)]
    pub storage_layouts: BTreeMap<Id, StorageLayout>,
}

impl RuntimeModel {
    /// Whether the runtime model declares nothing at all.
    pub fn is_empty(&self) -> bool {
        self.topics.is_empty()
            && self.subscriptions.is_empty()
            && self.execution_pools.is_empty()
            && self.routers.is_empty()
            && self.storage_layouts.is_empty()
    }
}

// ---------------------------------------------------------------------
// Topic transport
// ---------------------------------------------------------------------

/// Runtime transport facts for one L0 topic, in topic-scoped mode.
///
/// The logical channel — which messages the topic carries and what
/// identifies one of them — stays in L0 [`Topic`](super::Topic). What
/// lives here are two independent realization facts: the equivalence
/// domains the transport groups messages into, and the precedence it
/// establishes among them.
///
/// A topic runtime declaring either fact puts the topic in
/// **topic-scoped** mode: every subscription of it observes these
/// transport semantics, and none may declare its own. A topic runtime
/// declaring neither leaves the topic in **subscription-scoped** mode,
/// where each [`SubscriptionRuntime`] states its own. The two modes are
/// exclusive — there is no inheritance, no override, and no fallback
/// (see [`Model::effective_grouping`](super::Model::effective_grouping)).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicRuntime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<GroupingKey>,

    /// `Some(None)` is an explicit "this transport orders nothing",
    /// which *is* a declaration and so selects the scope; absent is no
    /// declaration at all. Collapsing the two would make an explicit
    /// negative at one scope silently inherit the other scope's
    /// precedence, which is the override this model forbids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering: Option<OrderingSemantics>,
}

impl TopicRuntime {
    /// Whether this runtime declares any transport fact at all, which
    /// is what puts the topic in topic-scoped mode.
    pub fn declares_transport_semantics(&self) -> bool {
        declares_transport_semantics(&self.grouping, &self.ordering)
    }
}

fn declares_transport_semantics(
    grouping: &Option<GroupingKey>,
    ordering: &Option<OrderingSemantics>,
) -> bool {
    grouping.is_some() || ordering.is_some()
}

/// Where a runtime grouping key lives in each grouped message schema.
///
/// Its presence *is* the grouping declaration: a transport either
/// groups by a key or it does not, so there is no separate "none"
/// spelling to write, and no way to write half a declaration.
///
/// A keyed grouping means
///
/// ```text
/// grouping_key(A) = grouping_key(B)  =>  runtime_group(A) = runtime_group(B)
/// ```
///
/// and nothing further — not ordering, not serialization, not member
/// assignment, not execution affinity, not uniqueness, not
/// storage-partition identity. Each of those needs its own declared
/// fact.
///
/// A grouping key's *fields* come from L0 message schemas, but the
/// statement "this runtime groups traffic by these fields" is a
/// realization fact, which is why grouping is L1. The key is semantic
/// in value and runtime-semantic in use; it is never a worker id,
/// partition number, shard id, host id, or storage-partition id.
///
/// Different schemas may map differently named fields into one domain,
/// so the mapping is per schema. Tuple positions correspond across
/// schemas, so every mapped tuple has the same arity — the rule
/// [`MessageIdentity`](super::MessageIdentity) already uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupingKey {
    pub mapping: BTreeMap<Id, Vec<FieldPath>>,
}

/// The precedence a transport establishes among messages, independent
/// of how it groups them.
///
/// Written as a bare value — `ordering: within_group`. Omitting the
/// field entirely is not the same as writing `none`: the first declares
/// nothing and leaves the scope open, the second is an explicit
/// negative that claims the scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderingSemantics {
    /// No usable transport precedence guarantee exists.
    #[default]
    None,

    /// One precedence relation across all messages in this scope.
    ///
    /// Stronger than same-group precedence, and it needs no grouping
    /// key of its own. It does not imply globally ordered *execution*:
    /// the execution topology must still preserve the precedence.
    Global,

    /// Precedence among messages belonging to the same declared
    /// grouping domain. Requires a grouping declaration at the same
    /// scope, since otherwise there is no domain the guarantee could
    /// be interpreted over.
    WithinGroup,
}



// ---------------------------------------------------------------------
// Subscription transport and dispatch
// ---------------------------------------------------------------------

/// Runtime facts for one L0 subscription input, addressed by the
/// `(operation, input)` pair the L0 model already establishes.
///
/// The L0 [`SubscriptionInput`](super::SubscriptionInput) says that a
/// logical message admitted through this subscription may invoke the
/// operation. This says how often the transport may deliver it and
/// where those deliveries execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionRuntime {
    /// Delivery semantics of the realized subscription.
    pub delivery: DeliverySemantics,

    /// Transport grouping for this subscription, in subscription-scoped
    /// mode only — absent when the subscribed topic declares
    /// topic-scoped semantics, which validation enforces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<GroupingKey>,

    /// Transport precedence for this subscription, in the same mode
    /// and under the same rule. As on a topic runtime, an explicit
    /// `none` is a declaration and absent is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering: Option<OrderingSemantics>,

    /// Where deliveries execute.
    pub dispatch: SubscriptionDispatch,
}

impl SubscriptionRuntime {
    /// Whether this runtime declares transport semantics of its own,
    /// which is only valid when the topic declares none.
    pub fn declares_transport_semantics(&self) -> bool {
        declares_transport_semantics(&self.grouping, &self.ordering)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverySemantics {
    /// The specification does not provide enough information
    /// to determine duplicate/loss behaviour.
    Unspecified,

    /// A message is delivered no more than once.
    ///
    /// Loss may be possible, but redelivery of the same logical
    /// message is not. This does not imply exactly-once execution.
    AtMostOnce,

    /// A successfully published logical message may be delivered
    /// more than once.
    ///
    /// No retry-count, retry-timing, or eventual-success guarantee is
    /// implied.
    AtLeastOnce,
}

/// Where subscription deliveries execute.
///
/// There is no lane abstraction and no lane-concurrency field:
/// execution concurrency belongs exclusively to
/// [`ExecutionPool::member_concurrency`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionDispatch {
    /// The execution pool deliveries are assigned to.
    pub pool: Id,

    /// How deliveries are assigned to a member of that pool.
    ///
    /// `None` is not a routing mode: it is the absence of any
    /// member-affinity fact. The analyzer then knows only the target
    /// population.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<SubscriptionRouting>,
}

/// A semantic routing key and the rule assigning its domains to pool
/// members — the same two components as [`RequestRouting`], differing
/// only in how the key is expressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionRouting {
    pub key: SubscriptionRoutingKey,
    pub member_assignment: MemberAssignment,
}

/// The semantic routing key of a subscription delivery.
///
/// Like a request routing key, this defines an invocation-equivalence
/// domain, not a physical execution identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionRoutingKey {
    /// Route by the effective grouping key — whichever scope declared
    /// it (§12).
    ///
    /// Naming the grouping rather than restating its field mapping is
    /// what gives the analyzer grouping/routing-domain identity for
    /// free, and keeps the two from drifting apart. It requires a keyed
    /// grouping to name.
    GroupingKey,
}

// ---------------------------------------------------------------------
// Request routing
// ---------------------------------------------------------------------

/// How invocations entering through one L0 request input are assigned
/// into an execution pool.
///
/// A router does not choose which operation executes — the L0 request
/// boundary already determines that. It answers only: which member of
/// the target pool owns the semantic routing domain of this request?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Router {
    /// The L0 request boundary this router serves.
    pub boundary: OperationInputRef,

    /// The execution pool its invocations are assigned to.
    pub pool: Id,

    /// `None` means no member-affinity fact exists: invocations of
    /// this boundary execute within the pool, and nothing more may be
    /// inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<RequestRouting>,
}

/// One L0 invocation boundary: the `(operation, input)` pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationInputRef {
    pub operation: Id,
    pub input: Id,
}

/// A semantic routing key and the rule assigning its domains to pool
/// members.
///
/// Deliberately a product type rather than an enum: a routing key and
/// a member assignment are independent semantic dimensions, not
/// alternative modes. There is no `unspecified`, `unconstrained`, or
/// `single_member` variant — absence of the enclosing routing block
/// already expresses epistemic absence, and a single domain for every
/// invocation is an ordinary key expression, not a routing category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRouting {
    /// Fields of the request input's schema whose values, as a tuple,
    /// identify the routing domain. Must be non-empty.
    ///
    /// Two invocations of this router belong to the same routing
    /// domain exactly when their evaluated tuples are equal. The tuple
    /// does not select a member; that is `member_assignment`'s job
    /// alone.
    pub key: Vec<FieldPath>,

    pub member_assignment: MemberAssignment,
}

/// How a semantic routing domain is assigned to a member of an
/// execution pool.
///
/// Any assignment used to establish keyed serialization must preserve
/// exclusive ownership through reassignment: if domain `K` moves from
/// member A to member B, a conforming runtime must not let A and B
/// execute `K` in a manner that violates the declared one-owner
/// semantics. Draining, leases, generation fencing, and coordinated
/// handoff are conforming mechanisms; Conseqa models the resulting
/// guarantee, not the mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemberAssignment {
    /// Equal routing domains are owned by the same execution-pool
    /// member during a stable ownership epoch. Different domains may
    /// share a member.
    ///
    /// The declaration fixes semantic assignment behaviour. It
    /// prescribes no hash function, virtual-node count, or membership
    /// discovery mechanism.
    ConsistentHash,

    /// Each invocation goes to the next member in rotation,
    /// irrespective of which routing domain it belongs to.
    ///
    /// No correctness proof consumes this, and it belongs here anyway.
    /// The analyzer is not L1's only reader: an external scenario
    /// evaluates the same declarations for hot members, routing skew
    /// and contention, and there this is decisive. Under a skewed key
    /// distribution consistent-hash concentrates load on the members
    /// owning the hot domains, while rotation spreads it evenly and
    /// gives up locality — same pool, same cardinality, different
    /// answer. A model that could not tell the two apart would hand
    /// that analysis a coin flip.
    ///
    /// It is also a *known arbitrary* assignment, which is not the same
    /// statement as declaring no routing at all. Omitting the routing
    /// block says nothing is known about member affinity; this says
    /// affinity is known not to exist, so a serialization or ordering
    /// requirement over the boundary is refused with a reason rather
    /// than for want of a declaration nobody has made.
    ///
    /// A routing key declared alongside it still names domains, and
    /// those domains still have their own identity; this assignment
    /// simply does not respect them.
    RoundRobin,
}

// ---------------------------------------------------------------------
// Execution topology
// ---------------------------------------------------------------------

/// A logical population of interchangeable runtime members capable of
/// executing the operation invocations assigned to that pool.
///
/// A pool establishes runtime population identity and the qualitative
/// execution concurrency of each member. It deliberately carries no
/// cardinality: member count, replica count, CPU, memory, autoscaling
/// rules, host and container counts are external scenario inputs, so
/// one Conseqa architecture can be evaluated against many of them.
///
/// Two paths targeting the same pool share one logical execution
/// population. Two paths targeting different pools target distinct
/// populations — which implies nothing about hosts, processes,
/// deployments, availability zones, or failure domains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPool {
    pub member_concurrency: MemberConcurrency,
}

/// How many invocations one member of a pool may execute at once.
///
/// This is the model's only runtime execution-concurrency primitive.
/// Routing determines where work executes; this determines how much
/// may execute there concurrently.
///
/// Unlike routing, member concurrency has genuine semantic value in
/// distinguishing an unknown resource from an explicitly unconstrained
/// one, so it keeps both `unspecified` and `unbounded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MemberConcurrency {
    /// No usable fact exists about simultaneous execution on one pool
    /// member.
    Unspecified,

    /// At most `n` operation invocations assigned to one member may be
    /// simultaneously active. The bound applies across every
    /// invocation assigned to that member, irrespective of operation
    /// or ingress mechanism.
    Bounded(NonZeroU32),

    /// No finite member-level execution bound may be assumed.
    Unbounded,
}

impl MemberConcurrency {
    /// Whether one member executes at most one invocation at a time.
    pub fn is_serial(self) -> bool {
        matches!(self, Self::Bounded(bound) if bound.get() == 1)
    }
}

// ---------------------------------------------------------------------
// Storage topology
// ---------------------------------------------------------------------

/// Physical partition identity for one L0 data object.
///
/// Object identity answers "which logical object instance is this?";
/// a storage layout answers "into which physical partition is that
/// instance mapped?". The two are independent, and the analyzer never
/// substitutes one for the other — nor equates a partition key with a
/// routing key whose field expression happens to coincide.
///
/// A layout implies nothing about database vendor, node count,
/// replication factor, replica placement, consistency level, partition
/// capacity, latency, or availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageLayout {
    pub object: DataObjectRef,

    /// Fields of the object's schema whose values, as a tuple,
    /// determine physical partition identity. Must be non-empty.
    pub partition_key: Vec<FieldPath>,
}

/// One L0 data object: the `(data_model, object)` pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataObjectRef {
    pub data_model: Id,
    pub object: Id,
}
