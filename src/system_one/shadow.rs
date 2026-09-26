//! The shadow comparator (§11.5 of the System One orchestration
//! revision).
//!
//! Two backends, one *primary* and one *shadow*. Every request goes to
//! both. Only the primary's answers drive behaviour; the shadow's are
//! carried beside them for the decision log, with whether the two
//! agreed.
//!
//! This is how a candidate backend — a local model, a new release of a
//! hosted one — is measured on the questions that matter, at the rate
//! they are really asked, without being trusted on the primary's
//! behalf. The shadow can only ever cost time: its failure is
//! recorded, never raised.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    Answer, Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest, QuestionId,
};

/// How long the shadow is waited for once the primary has answered.
const SHADOW_GRACE: Duration = Duration::from_secs(30);

/// Whether two backends gave the same answer to one question.
///
/// Agreement is measured on what a builder would act on — the chosen
/// option, the side of one half, the level — and the distance is kept
/// so a threshold can be moved later without asking again (§13.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Agreement {
    Choice {
        same_choice: bool,

        /// The shadow's probability for the option the primary chose.
        shadow_probability_of_primary_choice: f64,
    },

    Noul {
        /// Whether both fall on the same side of one half.
        same_side: bool,
        distance: f64,
    },

    Score {
        /// Whether both round to the same level.
        same_level: bool,
        distance: f64,
    },

    /// The shadow answered with a different type, or not at all.
    Incomparable,
}

impl Agreement {
    pub fn between(primary: &Answer, shadow: &Answer) -> Self {
        match (primary, shadow) {
            (
                Answer::Choice { choice, .. },
                Answer::Choice {
                    choice: shadow_choice,
                    probabilities,
                    ..
                },
            ) => Self::Choice {
                same_choice: choice == shadow_choice,
                shadow_probability_of_primary_choice: probabilities
                    .get(choice)
                    .copied()
                    .unwrap_or(0.0),
            },

            (Answer::Noul { noul }, Answer::Noul { noul: shadow_noul }) => Self::Noul {
                same_side: (*noul >= 0.5) == (*shadow_noul >= 0.5),
                distance: (noul - shadow_noul).abs(),
            },

            (
                Answer::Score { score, .. },
                Answer::Score {
                    score: shadow_score,
                    ..
                },
            ) => Self::Score {
                same_level: score.round() == shadow_score.round(),
                distance: (score - shadow_score).abs(),
            },

            _ => Self::Incomparable,
        }
    }

    /// Whether a builder acting on either answer would have acted the
    /// same way.
    pub fn agrees(&self) -> bool {
        match self {
            Self::Choice { same_choice, .. } => *same_choice,
            Self::Noul { same_side, .. } => *same_side,
            Self::Score { same_level, .. } => *same_level,
            Self::Incomparable => false,
        }
    }
}

/// What the shadow backend made of the request the primary answered.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowOutcome {
    pub identity: DeciderIdentity,

    /// The shadow's answers, or why it gave none.
    pub answers: Result<BTreeMap<QuestionId, Answer>, String>,

    /// Per question, for every question both backends answered.
    pub agreement: BTreeMap<QuestionId, Agreement>,

    pub latency: Option<Duration>,
}

/// A decider that answers with its primary and records its shadow.
pub struct ShadowDecider {
    primary: Arc<dyn Decider>,
    shadow: Arc<dyn Decider>,
}

impl std::fmt::Debug for ShadowDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowDecider")
            .field("primary", &self.primary.identity())
            .field("shadow", &self.shadow.identity())
            .finish()
    }
}

impl ShadowDecider {
    pub fn new(primary: Arc<dyn Decider>, shadow: Arc<dyn Decider>) -> Self {
        Self { primary, shadow }
    }
}

#[async_trait]
impl Decider for ShadowDecider {
    /// The primary's identity: it is the primary whose answers are
    /// acted on, and whose thresholds apply.
    fn identity(&self) -> DeciderIdentity {
        self.primary.identity()
    }

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
        let shadowed = async {
            tokio::time::timeout(SHADOW_GRACE, self.shadow.decide(request))
                .await
                .unwrap_or_else(|_| {
                    Err(DeciderError::Unavailable {
                        attempts: 1,
                        last: format!("no answer within {SHADOW_GRACE:?}"),
                    })
                })
        };

        let (primary, shadow) = tokio::join!(self.primary.decide(request), shadowed);

        // Without a primary answer there is nothing to compare the
        // shadow with, and nothing it could be allowed to replace.
        let mut decision = primary?;

        let outcome = match shadow {
            Ok(shadow) => ShadowOutcome {
                identity: shadow.identity,
                agreement: decision
                    .answers
                    .iter()
                    .filter_map(|(id, answer)| {
                        shadow
                            .answers
                            .get(id)
                            .map(|other| (id.clone(), Agreement::between(answer, other)))
                    })
                    .collect(),
                latency: Some(shadow.latency),
                answers: Ok(shadow.answers),
            },

            Err(error) => ShadowOutcome {
                identity: self.shadow.identity(),
                answers: Err(error.to_string()),
                agreement: BTreeMap::new(),
                latency: None,
            },
        };

        decision.shadow = Some(outcome);

        Ok(decision)
    }
}
