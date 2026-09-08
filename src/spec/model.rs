use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::spec::StateMachine;

use super::{
    DataModel, DeliverySemantics, ExecutionPool, Id, Operation, Router, RuntimeModel, Schema,
    Service, SubscriptionRuntime, Topic, TopicOrdering, TopicRuntime,
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

    /// The topic's declared transport ordering.
    ///
    /// An undeclared topic runtime is `Unspecified`: the two are the
    /// same epistemic position — no usable ordering fact — so the
    /// analyzer needs no separate case for a missing L1.
    pub fn topic_ordering(&self, topic: &Id) -> TopicOrdering {
        self.topic_runtime(topic)
            .map(|runtime| runtime.ordering.clone())
            .unwrap_or(TopicOrdering::Unspecified)
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

    /// The router serving one request boundary, with its ID.
    ///
    /// Validation admits at most one router per boundary, so the first
    /// match in canonical order is the only one.
    pub fn router_for(&self, operation: &Id, input: &Id) -> Option<(&Id, &Router)> {
        self.runtime.as_ref()?.routers.iter().find(|(_, router)| {
            &router.boundary.operation == operation && &router.boundary.input == input
        })
    }

    /// The named execution pool, if declared.
    pub fn execution_pool(&self, pool: &Id) -> Option<&ExecutionPool> {
        self.runtime.as_ref()?.execution_pools.get(pool)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);
