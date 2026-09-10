use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Service,
    Schema,

    DataModel,
    DataObject,
    Outbox,

    Topic,

    StateMachine,
    State,
    Transition,

    Operation,
    Input,
    Effect,
    EffectIntent,
    TransactionOutput,
    Transaction,
    TransactionRead,

    /// A result binding declared by an effect-executing program step.
    EffectResult,

    /// An asynchronous-execution handle bound by an async launch step,
    /// consumable only by synchronization steps.
    AsyncHandle,

    // L1 — runtime topology.
    ExecutionPool,
    Router,
    StorageLayout,
}

impl fmt::Display for ReferenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Service => "service",
            Self::Schema => "schema",

            Self::DataModel => "data model",
            Self::DataObject => "data object",
            Self::Outbox => "outbox",

            Self::Topic => "topic",

            Self::StateMachine => "state machine",
            Self::State => "state",
            Self::Transition => "transition",

            Self::Operation => "operation",
            Self::Input => "input",
            Self::Effect => "effect",
            Self::EffectIntent => "effect intent",
            Self::TransactionOutput => "transaction output",
            Self::Transaction => "transaction",
            Self::TransactionRead => "transaction read",
            Self::EffectResult => "effect result binding",
            Self::AsyncHandle => "async handle",

            Self::ExecutionPool => "execution pool",
            Self::Router => "router",
            Self::StorageLayout => "storage layout",
        };

        f.write_str(name)
    }
}
