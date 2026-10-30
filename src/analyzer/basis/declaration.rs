//! Declaration keys: the members of the declaration partition, the
//! units a semantic change and a fact's anchors are attributed to (§7.2
//! and §10.9 of the spec change analysis revision).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::spec::Id;

use super::{ObligationKey, ObligationProperty, RequirementFamily, RequirementIdentity};

/// One member of the declaration partition (§7.2).
///
/// The structured form is the `kind` / `value` map of the diff
/// document (§14), the shape the design layer's symbol keys also take.
/// The textual form is what a person writes by hand, as Part IV's code
/// markers do: the member kind, a space, and the declaration's ids,
/// each escaped as the store escapes a file name and joined by `/`
/// (§10.9). Neither `/` nor a space can appear inside an escaped id, so
/// every declaration has exactly one textual key, except a requirement,
/// which no one writes by hand.
///
/// The variants follow §7.2's order, L0 then L1 then intent, so the
/// derived order is the order a diff lists changes in (§8.4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DeclarationKey {
    // ---- L0 ----
    Service(Id),
    Schema(Id),
    DataModel(Id),
    DataObject {
        data_model: Id,
        object: Id,
    },
    Outbox {
        data_model: Id,
        outbox: Id,
    },
    Topic(Id),
    StateMachine(Id),
    Transition {
        machine: Id,
        transition: Id,
    },
    Operation(Id),
    Input {
        operation: Id,
        input: Id,
    },

    /// An operation's program: its control structure, with each inline
    /// transaction and effect contract owned by its own member.
    Program(Id),

    Transaction {
        operation: Id,
        transaction: Id,
    },
    EffectSite {
        operation: Id,
        effect: Id,
    },
    Requirement(RequirementIdentity),

    // ---- L1 ----
    TopicRuntime(Id),
    SubscriptionRuntime {
        operation: Id,
        input: Id,
    },
    OutboxRuntime {
        operation: Id,
        input: Id,
    },
    ExecutionPool(Id),
    Router(Id),
    StorageLayout(Id),

    // ---- intent ----
    PromptObligation(Id),
}

/// The member kinds with a textual form, and how many ids each names.
const TEXTUAL_KINDS: [(&str, usize); 20] = [
    ("service", 1),
    ("schema", 1),
    ("data_model", 1),
    ("data_object", 2),
    ("outbox", 2),
    ("topic", 1),
    ("state_machine", 1),
    ("transition", 2),
    ("operation", 1),
    ("input", 2),
    ("program", 1),
    ("transaction", 2),
    ("effect_site", 2),
    ("topic_runtime", 1),
    ("subscription_runtime", 2),
    ("outbox_runtime", 2),
    ("execution_pool", 1),
    ("router", 1),
    ("storage_layout", 1),
    ("prompt_obligation", 1),
];

impl DeclarationKey {
    /// The member kind, spelled as both forms spell it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Service(_) => "service",
            Self::Schema(_) => "schema",
            Self::DataModel(_) => "data_model",
            Self::DataObject { .. } => "data_object",
            Self::Outbox { .. } => "outbox",
            Self::Topic(_) => "topic",
            Self::StateMachine(_) => "state_machine",
            Self::Transition { .. } => "transition",
            Self::Operation(_) => "operation",
            Self::Input { .. } => "input",
            Self::Program(_) => "program",
            Self::Transaction { .. } => "transaction",
            Self::EffectSite { .. } => "effect_site",
            Self::Requirement(_) => "requirement",
            Self::TopicRuntime(_) => "topic_runtime",
            Self::SubscriptionRuntime { .. } => "subscription_runtime",
            Self::OutboxRuntime { .. } => "outbox_runtime",
            Self::ExecutionPool(_) => "execution_pool",
            Self::Router(_) => "router",
            Self::StorageLayout(_) => "storage_layout",
            Self::PromptObligation(_) => "prompt_obligation",
        }
    }

    /// The declaration's ids in textual order, the containing
    /// declaration's first, or `None` for a requirement.
    pub fn ids(&self) -> Option<Vec<&Id>> {
        Some(match self {
            Self::Service(id)
            | Self::Schema(id)
            | Self::DataModel(id)
            | Self::Topic(id)
            | Self::StateMachine(id)
            | Self::Operation(id)
            | Self::Program(id)
            | Self::TopicRuntime(id)
            | Self::ExecutionPool(id)
            | Self::Router(id)
            | Self::StorageLayout(id)
            | Self::PromptObligation(id) => vec![id],

            Self::DataObject { data_model, object } => vec![data_model, object],
            Self::Outbox { data_model, outbox } => vec![data_model, outbox],
            Self::Transition {
                machine,
                transition,
            } => vec![machine, transition],

            Self::Input { operation, input }
            | Self::SubscriptionRuntime { operation, input }
            | Self::OutboxRuntime { operation, input } => vec![operation, input],

            Self::Transaction {
                operation,
                transaction,
            } => vec![operation, transaction],

            Self::EffectSite { operation, effect } => vec![operation, effect],

            Self::Requirement(_) => return None,
        })
    }

    /// The textual key, or `None` for a requirement, which has none.
    pub fn textual(&self) -> Option<String> {
        let ids = self.ids()?;

        Some(format!(
            "{} {}",
            self.kind(),
            ids.iter()
                .map(|id| id.encoded())
                .collect::<Vec<_>>()
                .join("/")
        ))
    }

    /// Parses a textual key. Only the canonical spelling is read: one
    /// space after the kind, the kind's exact number of ids, and every
    /// id in its canonical escaping, so one declaration never has two
    /// spellings.
    pub fn parse(text: &str) -> Result<Self, TextualKeyError> {
        let Some((kind, ids)) = text.split_once(' ') else {
            return Err(TextualKeyError::Malformed {
                text: text.to_string(),
            });
        };

        if kind == "requirement" {
            return Err(TextualKeyError::NoTextualForm {
                kind: kind.to_string(),
            });
        }

        let Some(&(kind, arity)) = TEXTUAL_KINDS.iter().find(|(name, _)| *name == kind) else {
            return Err(TextualKeyError::UnknownKind {
                kind: kind.to_string(),
            });
        };

        let segments: Vec<&str> = ids.split('/').collect();

        if segments.len() != arity {
            return Err(TextualKeyError::WrongArity {
                kind: kind.to_string(),
                expected: arity,
                found: segments.len(),
            });
        }

        let mut ids = Vec::with_capacity(arity);

        for segment in segments {
            ids.push(
                Id::decode(segment).ok_or_else(|| TextualKeyError::NotCanonical {
                    segment: segment.to_string(),
                })?,
            );
        }

        let mut ids = ids.into_iter();
        let mut next = || ids.next().expect("arity checked above");

        Ok(match kind {
            "service" => Self::Service(next()),
            "schema" => Self::Schema(next()),
            "data_model" => Self::DataModel(next()),
            "data_object" => Self::DataObject {
                data_model: next(),
                object: next(),
            },
            "outbox" => Self::Outbox {
                data_model: next(),
                outbox: next(),
            },
            "topic" => Self::Topic(next()),
            "state_machine" => Self::StateMachine(next()),
            "transition" => Self::Transition {
                machine: next(),
                transition: next(),
            },
            "operation" => Self::Operation(next()),
            "input" => Self::Input {
                operation: next(),
                input: next(),
            },
            "program" => Self::Program(next()),
            "transaction" => Self::Transaction {
                operation: next(),
                transaction: next(),
            },
            "effect_site" => Self::EffectSite {
                operation: next(),
                effect: next(),
            },
            "topic_runtime" => Self::TopicRuntime(next()),
            "subscription_runtime" => Self::SubscriptionRuntime {
                operation: next(),
                input: next(),
            },
            "outbox_runtime" => Self::OutboxRuntime {
                operation: next(),
                input: next(),
            },
            "execution_pool" => Self::ExecutionPool(next()),
            "router" => Self::Router(next()),
            "storage_layout" => Self::StorageLayout(next()),
            "prompt_obligation" => Self::PromptObligation(next()),

            other => unreachable!("`{other}` is listed in TEXTUAL_KINDS"),
        })
    }
}

/// The textual key, or for a requirement a reading of its identity.
impl fmt::Display for DeclarationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.textual() {
            Some(text) => f.write_str(&text),

            None => {
                let Self::Requirement(identity) = self else {
                    unreachable!("only a requirement has no textual form")
                };

                let key = ObligationKey {
                    subject: identity.subject.clone(),
                    property: match identity.family {
                        RequirementFamily::TransactionSerializability => {
                            ObligationProperty::TransactionSerializability
                        }
                        RequirementFamily::TransactionOrdering => {
                            ObligationProperty::TransactionOrdering
                        }
                        RequirementFamily::Idempotency => ObligationProperty::Idempotency,
                        RequirementFamily::Recoverability => ObligationProperty::Recoverability,
                    },
                    requirement_key: identity.requirement_key.clone(),
                    occurrence: identity.occurrence,
                };

                write!(f, "requirement {key}")
            }
        }
    }
}

/// Why a textual declaration key was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextualKeyError {
    /// No space separates the kind from the ids.
    Malformed { text: String },

    /// The kind is not a member kind of the declaration partition.
    UnknownKind { kind: String },

    /// The kind has no textual form.
    NoTextualForm { kind: String },

    /// The kind names a different number of ids.
    WrongArity {
        kind: String,
        expected: usize,
        found: usize,
    },

    /// A segment is not an id's canonical escaping.
    NotCanonical { segment: String },
}

impl fmt::Display for TextualKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { text } => write!(
                f,
                "`{text}` is not a declaration key: write the member kind, a space, and the \
                 declaration's ids joined by `/`"
            ),

            Self::UnknownKind { kind } => {
                write!(
                    f,
                    "`{kind}` is not a member kind of the declaration partition"
                )
            }

            Self::NoTextualForm { kind } => {
                write!(f, "a `{kind}` declaration has no textual key")
            }

            Self::WrongArity {
                kind,
                expected,
                found,
            } => write!(
                f,
                "a `{kind}` key names {expected} id{}, not {found}",
                if *expected == 1 { "" } else { "s" }
            ),

            Self::NotCanonical { segment } => write!(
                f,
                "`{segment}` is not an id's canonical escaping: letters, digits, `_`, `-`, \
                 and `.` (not leading) as themselves, every other byte as `%XX`"
            ),
        }
    }
}

impl std::error::Error for TextualKeyError {}

/// A declaration key that has a textual form, which is how it is
/// written wherever it appears: every member but a requirement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextualKey(DeclarationKey);

impl TextualKey {
    /// The key, if it has a textual form.
    pub fn new(key: DeclarationKey) -> Option<Self> {
        key.textual().is_some().then_some(Self(key))
    }

    pub fn key(&self) -> &DeclarationKey {
        &self.0
    }

    pub fn into_key(self) -> DeclarationKey {
        self.0
    }
}

impl fmt::Display for TextualKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TextualKey {
    type Err = TextualKeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        DeclarationKey::parse(text).map(Self)
    }
}

impl Serialize for TextualKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TextualKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;

        text.parse().map_err(serde::de::Error::custom)
    }
}
