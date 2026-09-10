pub mod outbox;
pub mod request;
pub mod subscription;

pub use outbox::*;
pub use request::*;
use serde::{Deserialize, Serialize};
pub use subscription::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Request(RequestInput),
    Subscription(SubscriptionInput),
    Outbox(OutboxInput),
}
