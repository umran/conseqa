//! The trigger graph: which modeled operations an effect execution
//! goes on to invoke.
//!
//! A request names its target directly. A publication reaches every
//! subscription on its topic whose message selection admits the
//! published schema; an outbox write reaches every outbox input on its
//! outbox whose selection admits the written schema — the model's
//! closed world of consumers. Verifiers that follow effects across
//! operations — idempotency's cascade today; ordering's precedence
//! source and process completion when they come — resolve those edges
//! here, once per model, so they all agree on what "downstream" means.

use std::collections::BTreeMap;

use crate::spec::{
    Effect, ExternalEffect, Id, IdempotencyKey, Input, MessageSelector, Model, Operation,
    OutboxInput, OutboxWriteEffect, PublicationEffect, RequestEffect, ResultReplayRequirement,
    SubscriptionInput, TransitionSideEffect, ValueSource,
};

/// A modeled consumer of messages on a topic: the operation and the
/// subscription input through which it consumes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Consumer<'a> {
    pub operation: &'a Id,
    pub input: &'a Id,
    pub subscription: &'a SubscriptionInput,
}

/// A modeled producer of messages on a topic: a publication effect and
/// the declaration that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Producer<'a> {
    pub site: ProducerSite<'a>,
    pub effect: &'a Id,
    pub publication: &'a PublicationEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerSite<'a> {
    /// An effect declared by an operation.
    Operation { operation: &'a Id },

    /// A side effect owned by a state-machine transition (§22).
    Transition { machine: &'a Id, transition: &'a Id },
}

/// A modeled consumer of messages admitted to an outbox: the operation
/// and the outbox input through which it consumes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxConsumer<'a> {
    pub operation: &'a Id,
    pub input: &'a Id,
    pub declaration: &'a OutboxInput,
}

/// A modeled producer of messages into an outbox: a transactional
/// outbox-write site and the operation and transaction that carry it.
/// Only operations produce outbox writes — transitions cannot declare
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxProducer<'a> {
    pub operation: &'a Id,
    pub transaction: &'a Id,
    pub effect: &'a Id,
    pub write: &'a OutboxWriteEffect,
}

#[derive(Debug)]
pub struct TriggerGraph<'a> {
    model: &'a Model,

    /// Subscriptions per topic, in model order.
    subscriptions: BTreeMap<&'a Id, Vec<Consumer<'a>>>,

    /// Publications per topic, in model order: operation effects, then
    /// transition side effects.
    publications: BTreeMap<&'a Id, Vec<Producer<'a>>>,

    /// Outbox inputs per outbox, in model order.
    outbox_inputs: BTreeMap<&'a Id, Vec<OutboxConsumer<'a>>>,

    /// Transactional outbox writes per outbox, in model order.
    outbox_writes: BTreeMap<&'a Id, Vec<OutboxProducer<'a>>>,
}

impl<'a> TriggerGraph<'a> {
    pub fn new(model: &'a Model) -> Self {
        let mut subscriptions: BTreeMap<&'a Id, Vec<Consumer<'a>>> = BTreeMap::new();

        for (operation, declaration) in &model.operations {
            for (input, declared) in &declaration.inputs {
                if let Input::Subscription(subscription) = declared {
                    subscriptions
                        .entry(&subscription.topic)
                        .or_default()
                        .push(Consumer {
                            operation,
                            input,
                            subscription,
                        });
                }
            }
        }

        let mut publications: BTreeMap<&'a Id, Vec<Producer<'a>>> = BTreeMap::new();

        for (operation, declaration) in &model.operations {
            for (effect, declared) in declaration.program.effect_declarations() {
                if let Effect::Publication(publication) = declared {
                    publications
                        .entry(&publication.topic)
                        .or_default()
                        .push(Producer {
                            site: ProducerSite::Operation { operation },
                            effect,
                            publication,
                        });
                }
            }
        }

        for (machine, declaration) in &model.state_machines {
            for (transition, declared) in &declaration.transitions {
                for (effect, side_effect) in &declared.side_effects {
                    if let TransitionSideEffect::Publication(publication) = side_effect {
                        publications
                            .entry(&publication.topic)
                            .or_default()
                            .push(Producer {
                                site: ProducerSite::Transition {
                                    machine,
                                    transition,
                                },
                                effect,
                                publication,
                            });
                    }
                }
            }
        }

        let mut outbox_inputs: BTreeMap<&'a Id, Vec<OutboxConsumer<'a>>> = BTreeMap::new();

        for (operation, declaration) in &model.operations {
            for (input, declared) in &declaration.inputs {
                if let Input::Outbox(outbox_input) = declared {
                    outbox_inputs
                        .entry(&outbox_input.outbox)
                        .or_default()
                        .push(OutboxConsumer {
                            operation,
                            input,
                            declaration: outbox_input,
                        });
                }
            }
        }

        let mut outbox_writes: BTreeMap<&'a Id, Vec<OutboxProducer<'a>>> = BTreeMap::new();

        for (operation, declaration) in &model.operations {
            for (transaction, write) in declaration.program.outbox_write_declarations() {
                outbox_writes
                    .entry(&write.effect.outbox)
                    .or_default()
                    .push(OutboxProducer {
                        operation,
                        transaction,
                        effect: &write.effect_id,
                        write: &write.effect,
                    });
            }
        }

        Self {
            model,
            subscriptions,
            publications,
            outbox_inputs,
            outbox_writes,
        }
    }

    /// The modeled producers of `schema` on `topic`, in model order.
    pub fn producers(&self, topic: &Id, schema: &Id) -> Vec<Producer<'a>> {
        self.publications
            .get(topic)
            .into_iter()
            .flatten()
            .filter(|producer| &producer.publication.schema == schema)
            .copied()
            .collect()
    }

    /// The modeled consumers of `schema` published to `topic`, in
    /// model order: every subscription on the topic whose message
    /// selection admits the schema. `All` admits what the topic
    /// declares.
    pub fn consumers(&self, topic: &Id, schema: &Id) -> Vec<Consumer<'a>> {
        let declared = self
            .model
            .topics
            .get(topic)
            .is_some_and(|topic| topic.messages.contains(schema));

        self.subscriptions
            .get(topic)
            .into_iter()
            .flatten()
            .filter(|consumer| match &consumer.subscription.messages {
                MessageSelector::All => declared,
                MessageSelector::Only(schemas) => schemas.contains(schema),
            })
            .copied()
            .collect()
    }

    /// The modeled producers of `schema` into `outbox`, in model
    /// order.
    pub fn outbox_producers(&self, outbox: &Id, schema: &Id) -> Vec<OutboxProducer<'a>> {
        self.outbox_writes
            .get(outbox)
            .into_iter()
            .flatten()
            .filter(|producer| &producer.write.schema == schema)
            .copied()
            .collect()
    }

    /// The modeled consumers of `schema` admitted to `outbox`, in
    /// model order: every outbox input on the outbox whose message
    /// selection admits the schema, under the same rules as topic
    /// consumers.
    pub fn outbox_consumers(&self, outbox: &Id, schema: &Id) -> Vec<OutboxConsumer<'a>> {
        let declared = self
            .model
            .outbox(outbox)
            .is_some_and(|(_, outbox)| outbox.messages.contains(schema));

        self.outbox_inputs
            .get(outbox)
            .into_iter()
            .flatten()
            .filter(|consumer| match &consumer.declaration.messages {
                MessageSelector::All => declared,
                MessageSelector::Only(schemas) => schemas.contains(schema),
            })
            .copied()
            .collect()
    }
}

/// Whether `operation` declares an idempotency requirement keyed
/// entirely from `input` — the declaration whose proof collapses
/// payload-equal invocations through that input into the work of one
/// logical invocation (a governing key names one input, §12).
pub fn collapses_duplicates(operation: &Operation, input: &Id) -> bool {
    operation
        .requirements
        .idempotency
        .iter()
        .any(|requirement| {
            !requirement.key.components.is_empty()
                && requirement
                    .key
                    .components
                    .iter()
                    .all(|component| component.source == ValueSource::Input(input.clone()))
        })
}

/// The requirement of `operation` that declares its result
/// replay-consistent for invocations through `input`: an idempotency
/// requirement keyed entirely from that input with
/// `result: replay_consistent`. Its proof is what lets a caller observe
/// one result across repeated, payload-equal requests.
pub fn returns_consistently(operation: &Operation, input: &Id) -> Option<usize> {
    operation
        .requirements
        .idempotency
        .iter()
        .position(|requirement| {
            requirement.result == ResultReplayRequirement::ReplayConsistent
                && !requirement.key.components.is_empty()
                && requirement
                    .key
                    .components
                    .iter()
                    .all(|component| component.source == ValueSource::Input(input.clone()))
        })
}

/// The effect contract behind an execution site, unifying
/// operation-owned inline effects, transition side effects, and
/// transactional outbox writes.
#[derive(Debug, Clone, Copy)]
pub enum EffectContract<'a> {
    Publication(&'a PublicationEffect),
    Request(&'a RequestEffect),
    External(&'a ExternalEffect),
    OutboxWrite(&'a OutboxWriteEffect),
}

impl<'a> From<&'a Effect> for EffectContract<'a> {
    fn from(effect: &'a Effect) -> Self {
        match effect {
            Effect::Publication(publication) => Self::Publication(publication),
            Effect::Request(request) => Self::Request(request),
            Effect::External(external) => Self::External(external),
            Effect::OutboxWrite(write) => Self::OutboxWrite(write),
        }
    }
}

impl<'a> From<&'a TransitionSideEffect> for EffectContract<'a> {
    fn from(effect: &'a TransitionSideEffect) -> Self {
        match effect {
            TransitionSideEffect::Publication(publication) => Self::Publication(publication),
            TransitionSideEffect::Request(request) => Self::Request(request),
        }
    }
}

/// The contract of an effect executed by `operation`: one of its own
/// inline effect declarations, resolved from the program, or a
/// transition side effect reached through one of its transition
/// applications.
pub fn effect_contract<'a>(
    model: &'a Model,
    operation: &'a Operation,
    effect: &Id,
) -> Option<EffectContract<'a>> {
    for (declared_id, declared) in operation.program.effect_declarations() {
        if declared_id == effect {
            return Some(EffectContract::from(declared));
        }
    }

    for (_, write) in operation.program.outbox_write_declarations() {
        if &write.effect_id == effect {
            return Some(EffectContract::OutboxWrite(&write.effect));
        }
    }

    for machine in model.state_machines.values() {
        for transition in machine.transitions.values() {
            if let Some(side_effect) = transition.side_effects.get(effect) {
                return Some(EffectContract::from(side_effect));
            }
        }
    }

    None
}

/// The triggering input of an admissible governing key: the input its
/// first component names. Inadmissible keys are judged by the replay
/// engine; this is only how verdicts are keyed by `(operation, input)`.
pub fn key_input(key: &IdempotencyKey) -> Option<&Id> {
    match &key.components.first()?.source {
        ValueSource::Input(input) => Some(input),
        _ => None,
    }
}
