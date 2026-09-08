//! Structural validation of the L1 runtime model.
//!
//! Every L1 declaration hangs off an L0 one: a topic runtime names a
//! topic, a subscription runtime names an `(operation, input)` pair
//! that must be a subscription, a router names one that must be a
//! request, a storage layout names a data object. Those anchors, the
//! execution pools routing terminates at, and the field paths keys are
//! written against are what this pass checks.
//!
//! It deliberately checks nothing about whether the declared topology
//! *proves* anything. Whether a keyed domain and a serial pool
//! discharge a serialization requirement is verification's judgment;
//! here a runtime model is valid whenever its references resolve and
//! its keys are well-formed.

use crate::spec::{Input, Model, RuntimeModel, SubscriptionRoutingKey, TopicOrdering};

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
    validate_subscription_runtimes(model, runtime, errors);
    validate_routers(model, runtime, errors);
    validate_storage_layouts(model, runtime, errors);
}

/// A topic runtime must target an existing topic, and a keyed ordering
/// must satisfy the same schema, path, and total-coverage rules the
/// key domain has always had: every carried schema is routed, and only
/// carried schemas are mapped.
fn validate_topic_runtimes(
    model: &Model,
    runtime: &RuntimeModel,
    index: &ReferenceIndex<'_>,
    errors: &mut Vec<ValidationError>,
) {
    for (topic_id, topic_runtime) in &runtime.topics {
        super::expect_reference(index, topic_id, topic_id, ReferenceKind::Topic, errors);

        let TopicOrdering::Keyed(key) = &topic_runtime.ordering else {
            continue;
        };

        for schema in key.mapping.keys() {
            super::expect_reference(index, topic_id, schema, ReferenceKind::Schema, errors);
        }

        let Some(topic) = model.topics.get(topic_id) else {
            continue;
        };

        for schema in key.mapping.keys() {
            if !topic.messages.contains(schema) {
                errors.push(ValidationError::TopicKeySchemaNotOnTopic {
                    topic: topic_id.clone(),
                    schema: schema.clone(),
                });
            }
        }

        // Unlike message identity, the ordering key must route every
        // carried message: an unmapped schema has no place in the
        // sequence.
        for schema in &topic.messages {
            if !key.mapping.contains_key(schema) {
                errors.push(ValidationError::TopicKeyMissingSchema {
                    topic: topic_id.clone(),
                    schema: schema.clone(),
                });
            }
        }

        for (schema, path) in &key.mapping {
            if !schema_path_resolves(model, schema, path) {
                errors.push(ValidationError::InvalidFieldPath {
                    subject: topic_id.clone(),
                    schema: schema.clone(),
                    path: path.clone(),
                });
            }
        }
    }
}

/// A subscription runtime must name an existing subscription input,
/// dispatch to a declared pool, and — for `topic_key` routing — have a
/// keyed topic domain to route by.
fn validate_subscription_runtimes(
    model: &Model,
    runtime: &RuntimeModel,
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

            let Some(routing) = &subscription_runtime.dispatch.routing else {
                continue;
            };

            match routing.key {
                // The initial model derives the subscription's routing
                // domain from the topic's keyed transport domain, so
                // that domain has to exist. Separating the two is a
                // later refactor.
                SubscriptionRoutingKey::TopicKey => {
                    let keyed = matches!(
                        model.topic_ordering(&subscription.topic),
                        TopicOrdering::Keyed(_)
                    );

                    if !keyed {
                        errors.push(ValidationError::TopicKeyRoutingWithoutKeyDomain {
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
    let mut seen: Vec<(&crate::spec::OperationInputRef, &crate::spec::Id)> = Vec::new();

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
    let mut seen: Vec<(&crate::spec::DataObjectRef, &crate::spec::Id)> = Vec::new();

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

fn expect_pool(
    model: &Model,
    subject: &crate::spec::Id,
    pool: &crate::spec::Id,
    errors: &mut Vec<ValidationError>,
) {
    if model.execution_pool(pool).is_none() {
        errors.push(ValidationError::UnknownReference {
            subject: subject.clone(),
            reference: pool.clone(),
            expected: ReferenceKind::ExecutionPool,
        });
    }
}
