use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::spec::StateMachine;

use super::{
    DataModel, DeliverySemantics, ExecutionPool, GroupingKey, Id, Operation, OrderingSemantics,
    Router, RuntimeModel, Schema, Service, SubscriptionRuntime, Topic, TopicRuntime,
};

/// One Conseqa model, in two semantic layers.
///
/// The L0 collections sit directly on the root, so a model that
/// describes only application behaviour stays concise. The L1
/// realization hangs off `runtime` and is entirely optional: an
/// L0-only model is structurally valid and analyzable, it simply has
/// fewer facts from which its obligations can be discharged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub revision: Revision,

    // ---- L0: the abstract application machine ----
    #[serde(default)]
    pub services: BTreeMap<Id, Service>,
    #[serde(default)]
    pub schemas: BTreeMap<Id, Schema>,
    #[serde(default)]
    pub data_models: BTreeMap<Id, DataModel>,
    #[serde(default)]
    pub topics: BTreeMap<Id, Topic>,

    #[serde(default)]
    pub state_machines: BTreeMap<Id, StateMachine>,
    #[serde(default)]
    pub operations: BTreeMap<Id, Operation>,

    // ---- L1: one runtime realization of that machine ----
    /// Absent means no runtime facts are declared, never that the
    /// realization lacks the properties L1 can express.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeModel>,
}

impl Model {
    /// The declared runtime facts for a topic, if any.
    pub fn topic_runtime(&self, topic: &Id) -> Option<&TopicRuntime> {
        self.runtime.as_ref()?.topics.get(topic)
    }

    /// Whether a topic declares transport semantics for all of its
    /// subscriptions, rather than leaving each to declare its own.
    ///
    /// This is the mode selector of §12: the two scopes are exclusive,
    /// and validation rejects a model that declares at both.
    pub fn topic_scoped_transport(&self, topic: &Id) -> bool {
        self.topic_runtime(topic)
            .is_some_and(TopicRuntime::declares_transport_semantics)
    }

    /// The grouping domain in force for one subscription.
    ///
    /// Resolved from exactly one scope. There is no fallback chain and
    /// no override rule: a topic that declares transport semantics
    /// supplies them to every subscription, and otherwise each
    /// subscription supplies its own. A model validates into one mode
    /// before analysis begins, so this is a lookup rather than a
    /// precedence decision.
    pub fn effective_grouping(
        &self,
        operation: &Id,
        input: &Id,
        topic: &Id,
    ) -> Option<GroupingKey> {
        if self.topic_scoped_transport(topic) {
            return self.topic_runtime(topic)?.grouping.clone();
        }

        self.subscription_runtime(operation, input)?.grouping.clone()
    }

    /// The transport precedence in force for one subscription,
    /// resolved from the same single scope as the grouping.
    pub fn effective_ordering(&self, operation: &Id, input: &Id, topic: &Id) -> OrderingSemantics {
        if self.topic_scoped_transport(topic) {
            return self
                .topic_runtime(topic)
                .and_then(|runtime| runtime.ordering)
                .unwrap_or_default();
        }

        self.subscription_runtime(operation, input)
            .and_then(|runtime| runtime.ordering)
            .unwrap_or_default()
    }

    /// The declared runtime facts for one subscription input, if any.
    pub fn subscription_runtime(
        &self,
        operation: &Id,
        input: &Id,
    ) -> Option<&SubscriptionRuntime> {
        self.runtime.as_ref()?.subscriptions.get(operation)?.get(input)
    }

    /// The subscription's declared delivery semantics.
    ///
    /// As with topic ordering, an undeclared subscription runtime is
    /// `Unspecified`: duplicate and loss behaviour is simply unknown.
    pub fn delivery(&self, operation: &Id, input: &Id) -> DeliverySemantics {
        self.subscription_runtime(operation, input)
            .map(|runtime| runtime.delivery)
            .unwrap_or(DeliverySemantics::Unspecified)
    }

    /// Every router serving one request boundary, with its ID.
    ///
    /// Validation admits at most one, but this returns them all rather
    /// than picking: a boundary routed two ways has no single set of
    /// routing facts, and silently choosing one would let an analysis
    /// prove from half a contradictory declaration.
    pub fn routers_for(&self, operation: &Id, input: &Id) -> Vec<(&Id, &Router)> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Vec::new();
        };

        runtime
            .routers
            .iter()
            .filter(|(_, router)| {
                &router.boundary.operation == operation && &router.boundary.input == input
            })
            .collect()
    }

    /// The named execution pool, if declared.
    pub fn execution_pool(&self, pool: &Id) -> Option<&ExecutionPool> {
        self.runtime.as_ref()?.execution_pools.get(pool)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);
