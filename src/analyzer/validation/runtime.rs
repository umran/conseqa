//! Structural validation of the L1 runtime model.
//!
//! Every L1 declaration hangs off an L0 one: a topic runtime names a
//! topic, a subscription runtime names an `(operation, input)` pair
//! that must be a subscription, a router names one that must be a
//! request, a storage layout names a data object. Those anchors, the
//! execution pools routing terminates at, and the field paths keys are
//! written against are what this pass checks.
//!
//! It also settles the declaration scope of transport semantics. For
//! every topic, grouping and ordering are declared either once at the
//! topic runtime or independently at each subscription runtime, never
//! at both — a structural invariant, so the analyzer never needs an
//! inheritance or override rule and every subscription's effective
//! semantics come from one unambiguous place.
//!
//! Only the cross-scope half of that needs checking. Within a scope,
//! each fact is its own presence or absence, so "groups without
//! ordering" is a complete declaration rather than half a pair, and
//! half a pair cannot be written at all.
//!
//! It deliberately checks nothing about whether the declared topology
//! *proves* anything. Whether a grouping domain and a serial pool
//! discharge a serialization requirement is verification's judgment;
//! here a runtime model is valid whenever its references resolve, its
//! keys are well-formed, and its transport semantics have one scope.

use std::collections::{BTreeMap, BTreeSet};

use crate::spec::{
    GroupingKey, Id, Input, MessageSelector, Model, OrderingSemantics, RuntimeModel,
    SubscriptionRoutingKey,
};

use super::InputKind;
use super::error::ValidationError;
use super::reference::ReferenceKind;
use super::{ReferenceIndex, schema_path_resolves};

/// Validates the runtime model, if one is declared.
pub(super) fn validate_runtime(
    model: &Model,
    index: &ReferenceIndex<'_>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(runtime) = &model.runtime else {
        return;
    };

    validate_topic_runtimes(model, runtime, index, errors);
    validate_subscription_runtimes(model, runtime, index, errors);
    validate_transport_scope(model, runtime, errors);
    validate_routers(model, runtime, errors);
    validate_storage_layouts(model, runtime, errors);
}

/// A topic runtime must target an existing topic, and its grouping key
/// must satisfy the schema, path, coverage, and arity rules.
fn validate_topic_runtimes(
    model: &Model,
    runtime: &RuntimeModel,
    index: &ReferenceIndex<'_>,
    errors: &mut Vec<ValidationError>,
) {
    for (topic_id, topic_runtime) in &runtime.topics {
        super::expect_reference(index, topic_id, topic_id, ReferenceKind::Topic, errors);

        // A topic-scoped grouping serves every subscription of the
        // topic, so it must cover every message the topic carries.
        let carried: BTreeSet<&Id> = model
            .topics
            .get(topic_id)
            .map(|topic| topic.messages.iter().collect())
            .unwrap_or_default();

        validate_grouping(
            model,
            index,
            topic_id,
            topic_id,
            topic_runtime.grouping.as_ref(),
            topic_runtime.ordering,
            &carried,
            errors,
        );
    }
}

/// The shape rules a grouping declaration must satisfy, and the
/// dependency `within_group` has on it.
///
/// `subject` is what a diagnostic points at — the topic or the input,
/// depending on the scope. `topic_id` is the topic whose carried
/// schemas the mapping is judged against, which is the same topic
/// either way.
#[allow(clippy::too_many_arguments)]
fn validate_grouping(
    model: &Model,
    index: &ReferenceIndex<'_>,
    subject: &Id,
    topic_id: &Id,
    grouping: Option<&GroupingKey>,
    ordering: Option<OrderingSemantics>,
    // The schemas this scope actually receives. A topic-scoped grouping
    // serves every subscription, so it must cover the whole topic; a
    // subscription-scoped one only has to group what its own selector
    // admits, and requiring more can be impossible on a heterogeneous
    // topic where the filtered-out schema has no comparable field.
    covers: &BTreeSet<&Id>,
    errors: &mut Vec<ValidationError>,
) {
    // `within_group` names the domain a grouping declares. Without one
    // there is nothing for the guarantee to be interpreted over.
    if ordering == Some(OrderingSemantics::WithinGroup) && grouping.is_none() {
        errors.push(ValidationError::WithinGroupWithoutGrouping {
            subject: subject.clone(),
        });
    }

    let Some(key) = grouping else {
        return;
    };

    for schema in key.mapping.keys() {
        super::expect_reference(index, subject, schema, ReferenceKind::Schema, errors);
    }

    let Some(topic) = model.topics.get(topic_id) else {
        return;
    };

    for schema in key.mapping.keys() {
        if !topic.messages.contains(schema) {
            errors.push(ValidationError::GroupingKeySchemaNotOnTopic {
                subject: subject.clone(),
                topic: topic_id.clone(),
                schema: schema.clone(),
            });
        }
    }

    // Every message this scope receives must land in some group; an
    // unmapped one would belong to none.
    for schema in covers {
        if !key.mapping.contains_key(*schema) {
            errors.push(ValidationError::GroupingKeyMissingSchema {
                subject: subject.clone(),
                topic: topic_id.clone(),
                schema: (*schema).clone(),
            });
        }
    }

    // Tuple positions correspond across schemas, so every mapped tuple
    // shares one arity. Empty tuples are reported on their own and
    // excluded from the baseline.
    let expected = key.mapping.values().map(Vec::len).find(|len| *len > 0);

    for (schema, tuple) in &key.mapping {
        if tuple.is_empty() {
            errors.push(ValidationError::EmptyGroupingKey {
                subject: subject.clone(),
                schema: schema.clone(),
            });

            continue;
        }

        if let Some(expected) = expected
            && tuple.len() != expected
        {
            errors.push(ValidationError::GroupingKeyArityMismatch {
                subject: subject.clone(),
                schema: schema.clone(),
                expected,
                actual: tuple.len(),
            });
        }

        for path in tuple {
            if !schema_path_resolves(model, schema, path) {
                errors.push(ValidationError::InvalidFieldPath {
                    subject: subject.clone(),
                    schema: schema.clone(),
                    path: path.clone(),
                });
            }
        }
    }
}

/// Transport semantics have exactly one declaration scope per topic.
///
/// A topic declaring grouping or ordering supplies them to every
/// subscription of it, and none of those subscriptions may declare its
/// own. Checked as a structural invariant rather than resolved by
/// precedence, so effective semantics are never a question of which
/// declaration wins.
fn validate_transport_scope(
    model: &Model,
    runtime: &RuntimeModel,
    errors: &mut Vec<ValidationError>,
) {
    let mut subscription_scoped: BTreeMap<&Id, Vec<(&Id, &Id)>> = BTreeMap::new();

    for (operation_id, inputs) in &runtime.subscriptions {
        for (input_id, subscription_runtime) in inputs {
            if !subscription_runtime.declares_transport_semantics() {
                continue;
            }

            let Some(Input::Subscription(subscription)) = model
                .operations
                .get(operation_id)
                .and_then(|operation| operation.inputs.get(input_id))
            else {
                continue;
            };

            subscription_scoped
                .entry(&subscription.topic)
                .or_default()
                .push((operation_id, input_id));
        }
    }

    for (topic_id, topic_runtime) in &runtime.topics {
        if !topic_runtime.declares_transport_semantics() {
            continue;
        }

        for (operation, input) in subscription_scoped.remove(topic_id).unwrap_or_default() {
            errors.push(ValidationError::TransportSemanticsAtBothScopes {
                topic: topic_id.clone(),
                operation: operation.clone(),
                input: input.clone(),
            });
        }
    }
}

/// A subscription runtime must name an existing subscription input,
/// dispatch to a declared pool, and — for `topic_key` routing — have a
/// keyed topic domain to route by.
fn validate_subscription_runtimes(
    model: &Model,
    runtime: &RuntimeModel,
    index: &ReferenceIndex<'_>,
    errors: &mut Vec<ValidationError>,
) {
    for (operation_id, inputs) in &runtime.subscriptions {
        let Some(operation) = model.operations.get(operation_id) else {
            errors.push(ValidationError::UnknownReference {
                subject: operation_id.clone(),
                reference: operation_id.clone(),
                expected: ReferenceKind::Operation,
            });

            continue;
        };

        for (input_id, subscription_runtime) in inputs {
            let subscription = match operation.inputs.get(input_id) {
                Some(Input::Subscription(subscription)) => subscription,

                Some(Input::Request(_)) => {
                    errors.push(ValidationError::InvalidInputKind {
                        subject: input_id.clone(),
                        input: input_id.clone(),
                        expected: InputKind::Subscription,
                        actual: InputKind::Request,
                    });

                    continue;
                }

                None => {
                    errors.push(ValidationError::UnknownReference {
                        subject: operation_id.clone(),
                        reference: input_id.clone(),
                        expected: ReferenceKind::Input,
                    });

                    continue;
                }
            };

            expect_pool(model, input_id, &subscription_runtime.dispatch.pool, errors);

            // A subscription-scoped grouping only has to cover what
            // this subscription admits.
            let admitted: BTreeSet<&Id> = match &subscription.messages {
                MessageSelector::Only(schemas) => schemas.iter().collect(),

                MessageSelector::All => model
                    .topics
                    .get(&subscription.topic)
                    .map(|topic| topic.messages.iter().collect())
                    .unwrap_or_default(),
            };

            validate_grouping(
                model,
                index,
                input_id,
                &subscription.topic,
                subscription_runtime.grouping.as_ref(),
                subscription_runtime.ordering,
                &admitted,
                errors,
            );

            let Some(routing) = &subscription_runtime.dispatch.routing else {
                continue;
            };

            match routing.key {
                // `grouping_key` names the effective grouping domain,
                // so one has to exist — at whichever scope declares it.
                SubscriptionRoutingKey::GroupingKey => {
                    let grouping =
                        model.effective_grouping(operation_id, input_id, &subscription.topic);

                    if grouping.is_none() {
                        errors.push(ValidationError::RoutingWithoutGrouping {
                            operation: operation_id.clone(),
                            input: input_id.clone(),
                            topic: subscription.topic.clone(),
                        });
                    }
                }
            }
        }
    }
}

/// A router must name an existing request boundary, target a declared
/// pool, and — when it routes — carry a non-empty key that resolves
/// against the request schema. One boundary has at most one router.
fn validate_routers(model: &Model, runtime: &RuntimeModel, errors: &mut Vec<ValidationError>) {
    let mut seen: Vec<(&crate::spec::OperationInputRef, &Id)> = Vec::new();

    for (router_id, router) in &runtime.routers {
        let boundary = &router.boundary;

        if let Some((_, first)) = seen.iter().find(|(existing, _)| *existing == boundary) {
            errors.push(ValidationError::DuplicateRouterForBoundary {
                first: (*first).clone(),
                second: router_id.clone(),
                operation: boundary.operation.clone(),
                input: boundary.input.clone(),
            });
        } else {
            seen.push((boundary, router_id));
        }

        expect_pool(model, router_id, &router.pool, errors);

        let Some(operation) = model.operations.get(&boundary.operation) else {
            errors.push(ValidationError::UnknownReference {
                subject: router_id.clone(),
                reference: boundary.operation.clone(),
                expected: ReferenceKind::Operation,
            });

            continue;
        };

        let request = match operation.inputs.get(&boundary.input) {
            Some(Input::Request(request)) => request,

            Some(Input::Subscription(_)) => {
                errors.push(ValidationError::InvalidInputKind {
                    subject: router_id.clone(),
                    input: boundary.input.clone(),
                    expected: InputKind::Request,
                    actual: InputKind::Subscription,
                });

                continue;
            }

            None => {
                errors.push(ValidationError::UnknownReference {
                    subject: router_id.clone(),
                    reference: boundary.input.clone(),
                    expected: ReferenceKind::Input,
                });

                continue;
            }
        };

        let Some(routing) = &router.routing else {
            continue;
        };

        if routing.key.is_empty() {
            errors.push(ValidationError::EmptyRoutingKey {
                router: router_id.clone(),
            });
        }

        for path in &routing.key {
            if !schema_path_resolves(model, &request.schema, path) {
                errors.push(ValidationError::InvalidFieldPath {
                    subject: router_id.clone(),
                    schema: request.schema.clone(),
                    path: path.clone(),
                });
            }
        }
    }
}

/// A storage layout must name an existing data object and carry a
/// non-empty partition key resolving against that object's schema. One
/// object has at most one primary layout.
fn validate_storage_layouts(
    model: &Model,
    runtime: &RuntimeModel,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen: Vec<(&crate::spec::DataObjectRef, &Id)> = Vec::new();

    for (layout_id, layout) in &runtime.storage_layouts {
        if let Some((_, first)) = seen
            .iter()
            .find(|(existing, _)| *existing == &layout.object)
        {
            errors.push(ValidationError::DuplicateStorageLayoutForObject {
                first: (*first).clone(),
                second: layout_id.clone(),
                data_model: layout.object.data_model.clone(),
                object: layout.object.object.clone(),
            });
        } else {
            seen.push((&layout.object, layout_id));
        }

        if layout.partition_key.is_empty() {
            errors.push(ValidationError::EmptyPartitionKey {
                layout: layout_id.clone(),
            });
        }

        let Some(data_model) = model.data_models.get(&layout.object.data_model) else {
            errors.push(ValidationError::UnknownReference {
                subject: layout_id.clone(),
                reference: layout.object.data_model.clone(),
                expected: ReferenceKind::DataModel,
            });

            continue;
        };

        let Some(object) = data_model.objects.get(&layout.object.object) else {
            errors.push(ValidationError::UnknownReference {
                subject: layout_id.clone(),
                reference: layout.object.object.clone(),
                expected: ReferenceKind::DataObject,
            });

            continue;
        };

        for path in &layout.partition_key {
            if !schema_path_resolves(model, &object.schema, path) {
                errors.push(ValidationError::InvalidFieldPath {
                    subject: layout_id.clone(),
                    schema: object.schema.clone(),
                    path: path.clone(),
                });
            }
        }
    }
}

fn expect_pool(model: &Model, subject: &Id, pool: &Id, errors: &mut Vec<ValidationError>) {
    if model.execution_pool(pool).is_none() {
        errors.push(ValidationError::UnknownReference {
            subject: subject.clone(),
            reference: pool.clone(),
            expected: ReferenceKind::ExecutionPool,
        });
    }
}
