//! Completeness claims (§9 of the conformance revision): statements
//! about the whole repository, re-checked on every derivation.
//!
//! Some facts no region can attest. `no_deleters(object.order)` is true
//! of the code only if no code deletes orders outside the transactions
//! the model declares. A conflict closure is true of the code only if no
//! unmodeled code writes what its transactions access, since an
//! unmodeled writer joins the real closure invisibly (§9.1). A
//! completeness claim states that every match of some patterns, in the
//! scanned files, lies in a region of a declared writer or publisher, or
//! in an excepted file (§9.2).
//!
//! A claim is not sound: patterns can miss. It is recorded, reviewed,
//! and re-checked on every derivation rather than trusted from the time
//! it was written (§9.3), which is the most a tool that parses no
//! language can offer.
//!
//! The declared writers and publishers come from the design graph's
//! `Writers` and `Publishers` queries, so a claim accepts the code of
//! exactly the declarations the model says do the work.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::Serialize;

use crate::analyzer::basis::{Channel, DeclarationKey, Fact, TextualKey, TransactionId};
use crate::design::graph::{EffectRef, SymbolGraph};
use crate::design::graph_query::{self, GraphQuery, QueryRow};
use crate::design::workspace::WorkspaceState;
use crate::spec::{Id, Transaction, TransactionStep};

use super::markers::Region;
use super::record::{ClaimScope, CompletenessClaim, pattern_error};

/// How many outside matches a claim that does not hold lists; the rest
/// are counted.
pub const LISTED_MATCHES: usize = 20;

/// A claim scope the reliance requires of one declaration's record
/// (§9.4), with the facts requiring it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequiredScope {
    /// The `data_object`, `topic`, or `outbox` whose record carries the
    /// claim.
    pub carrier: TextualKey,

    pub scope: ClaimScope,

    /// The facts of the reliance requiring it, by canonical form.
    pub because: Vec<Fact>,
}

/// The claim scopes the reliance requires (§9.4), by carrier, then
/// scope.
pub fn required<'a>(
    reliance: impl IntoIterator<Item = &'a Fact>,
    workspace: &WorkspaceState,
) -> Vec<RequiredScope> {
    let mut required: BTreeMap<(TextualKey, ClaimScope), BTreeMap<String, Fact>> = BTreeMap::new();

    for fact in reliance {
        for (carrier, scope) in scopes(fact, workspace) {
            required
                .entry((carrier, scope))
                .or_default()
                .insert(fact.canonical(), fact.clone());
        }
    }

    required
        .into_iter()
        .map(|((carrier, scope), because)| RequiredScope {
            carrier,
            scope,
            because: because.into_values().collect(),
        })
        .collect()
}

/// The claim scopes one fact requires (§9.4):
///
/// - `no_deleters`, `writers` completeness of its object;
/// - a conflict closure, and the cyclic components of its dependency
///   graph, `writers` completeness of every object their transactions
///   read, write, or lock, since an unmodeled writer of any of them
///   joins the closure;
/// - a message identity, `publishers` completeness of its channel,
///   since an unbound publisher could put a second message with an
///   existing identity on it.
///
/// Every other fact requires none.
pub fn scopes(fact: &Fact, workspace: &WorkspaceState) -> Vec<(TextualKey, ClaimScope)> {
    let carriers: Vec<DeclarationKey> = match fact {
        Fact::NoDeleters { data_model, object } => vec![DeclarationKey::DataObject {
            data_model: data_model.clone(),
            object: object.clone(),
        }],

        Fact::ConflictClosure { members, .. } => accessed(workspace, members.iter()),

        Fact::CyclicComponents { components, .. } => {
            accessed(workspace, components.iter().flatten())
        }

        Fact::MessageIdentity { channel, .. } => {
            return vec![(channel_key(channel), ClaimScope::Publishers)];
        }

        Fact::Isolation { .. }
        | Fact::Lock { .. }
        | Fact::GuardedMutation { .. }
        | Fact::Cursor { .. }
        | Fact::Fence { .. }
        | Fact::KeyedRequirement { .. }
        | Fact::AdmitsNoMessages { .. }
        | Fact::NoAdmittedPaths { .. }
        | Fact::ReturnsNoResult { .. }
        | Fact::Terminal { .. }
        | Fact::StableRoot { .. }
        | Fact::Delivery { .. }
        | Fact::DeduplicatedBy { .. }
        | Fact::NaturallyReplayable { .. }
        | Fact::ArtifactRoute { .. }
        | Fact::OutboxRedrive { .. }
        | Fact::RetryMayRepeat { .. }
        | Fact::ExternalGuarantee { .. }
        | Fact::Propagation { .. }
        | Fact::ReliesOn { .. }
        | Fact::Realization { .. } => Vec::new(),
    };

    carriers
        .into_iter()
        .filter_map(TextualKey::new)
        .map(|carrier| (carrier, ClaimScope::Writers))
        .collect()
}

/// The objects the transactions read, write, or lock, as `data_object`
/// declarations.
fn accessed<'a>(
    workspace: &WorkspaceState,
    members: impl Iterator<Item = &'a TransactionId>,
) -> Vec<DeclarationKey> {
    let mut objects = BTreeSet::new();

    for member in members {
        let transaction = workspace
            .operations
            .get(&member.operation)
            .and_then(|operation| operation.program.as_ref())
            .and_then(|program| program.transaction(&member.transaction));

        let Some(Transaction {
            data_model: Some(data_model),
            steps,
            ..
        }) = transaction
        else {
            continue;
        };

        for object in steps.iter().filter_map(object) {
            objects.insert(DeclarationKey::DataObject {
                data_model: data_model.clone(),
                object: object.clone(),
            });
        }
    }

    objects.into_iter().collect()
}

/// The object a transaction step reads, writes, or locks.
fn object(step: &TransactionStep) -> Option<&Id> {
    match step {
        TransactionStep::Read(read) => Some(&read.target.object),
        TransactionStep::Update(update) => Some(&update.target.object),
        TransactionStep::CompareAndSet(cas) => Some(&cas.target.object),
        TransactionStep::Insert(insert) => Some(&insert.object),
        TransactionStep::Upsert(upsert) => Some(&upsert.target.object),
        TransactionStep::Delete(delete) => Some(&delete.target.object),
        TransactionStep::Lock(lock) => Some(&lock.target.object),
        TransactionStep::Transition(transition) => Some(&transition.subject.object),
        TransactionStep::AdvanceCursor(advance) => Some(&advance.target.object),
        TransactionStep::Fence(fence) => Some(&fence.target.object),

        // An artifact touches no object, and an outbox admission writes
        // a channel, which has a publishers claim of its own.
        TransactionStep::EstablishEffectIntent(_)
        | TransactionStep::EstablishTransactionOutput(_)
        | TransactionStep::WriteOutbox(_) => None,
    }
}

fn channel_key(channel: &Channel) -> TextualKey {
    let key = match channel {
        Channel::Topic { topic } => DeclarationKey::Topic(topic.clone()),

        Channel::Outbox { data_model, outbox } => DeclarationKey::Outbox {
            data_model: data_model.clone(),
            outbox: outbox.clone(),
        },
    };

    TextualKey::new(key).expect("a topic and an outbox have textual keys")
}

/// The declarations in whose regions a claim of `scope` on `carrier`
/// accepts matches (§9.2): the transactions the model declares as
/// writing the object, or the effect sites and transitions it declares
/// as publishing to, or writing into, the channel.
pub fn declared(
    scope: ClaimScope,
    carrier: &DeclarationKey,
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
) -> BTreeSet<DeclarationKey> {
    let query = match (scope, carrier) {
        (ClaimScope::Writers, DeclarationKey::DataObject { data_model, object }) => {
            GraphQuery::Writers {
                data_model: data_model.clone(),
                object: object.clone(),
                field: None,
            }
        }

        // The publishers index is keyed by the channel's id, a topic's
        // or an outbox's alike.
        (ClaimScope::Publishers, DeclarationKey::Topic(channel))
        | (
            ClaimScope::Publishers,
            DeclarationKey::Outbox {
                outbox: channel, ..
            },
        ) => GraphQuery::Publishers {
            topic: channel.clone(),
        },

        // The record format puts no other claim on any other kind.
        _ => return BTreeSet::new(),
    };

    graph_query::run(workspace, graph, &query)
        .rows
        .into_iter()
        .filter_map(|row| match row {
            QueryRow::Access(access) => Some(DeclarationKey::Transaction {
                operation: access.transaction.operation,
                transaction: access.transaction.transaction,
            }),

            QueryRow::Publisher(publisher) => Some(site_declaration(publisher.site)),

            _ => None,
        })
        .collect()
}

/// The declaration owning an effect site of the design graph: the
/// effect site, or the transition whose side effect it is.
pub fn site_declaration(site: EffectRef) -> DeclarationKey {
    match site {
        EffectRef::Operation { operation, effect } => {
            DeclarationKey::EffectSite { operation, effect }
        }

        EffectRef::Transition {
            machine,
            transition,
            effect: _,
        } => DeclarationKey::Transition {
            machine,
            transition,
        },
    }
}

/// A match of a claim's pattern: the file, and the first and last line
/// its text touches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Match {
    pub path: String,
    pub lines: [usize; 2],
}

/// The claims a derivation re-checks, compiled, collecting each one's
/// matches as the scan reads the files.
///
/// A pattern runs over a file's whole text as written, so a match may
/// span lines, as a SQL statement often does. A match of no text
/// touches no line, and is passed over.
#[derive(Debug)]
pub struct Matcher {
    claims: Vec<Vec<Regex>>,
    matches: Vec<BTreeSet<Match>>,
}

impl Matcher {
    /// Compiles every claim's patterns, in the order the claims are
    /// given. A record's reader refuses a pattern that does not compile,
    /// so the claims of a record that was read always do.
    pub fn new<'a>(
        claims: impl IntoIterator<Item = &'a CompletenessClaim>,
    ) -> Result<Self, String> {
        let mut compiled = Vec::new();

        for claim in claims {
            let mut patterns = Vec::with_capacity(claim.patterns.len());

            for pattern in &claim.patterns {
                patterns.push(Regex::new(pattern).map_err(|error| {
                    format!(
                        "`{pattern}` is not a regular expression: {}",
                        pattern_error(&error)
                    )
                })?);
            }

            compiled.push(patterns);
        }

        Ok(Self {
            matches: vec![BTreeSet::new(); compiled.len()],
            claims: compiled,
        })
    }

    /// Matches every claim against one file's text.
    pub fn visit(&mut self, path: &str, text: &str) {
        // Where each line starts, computed for the first file that
        // matches anything: most files match nothing.
        let mut starts: Option<Vec<usize>> = None;

        for (claim, patterns) in self.claims.iter().enumerate() {
            for pattern in patterns {
                for found in pattern.find_iter(text) {
                    if found.is_empty() {
                        continue;
                    }

                    let starts = starts.get_or_insert_with(|| line_starts(text));

                    // The line of a byte is the number of lines starting
                    // at or before it. A line's `\n` is its own.
                    let line = |offset: usize| starts.partition_point(|&start| start <= offset);

                    self.matches[claim].insert(Match {
                        path: path.to_string(),
                        lines: [line(found.start()), line(found.end() - 1)],
                    });
                }
            }
        }
    }

    /// Each claim's matches, by path, then line, in the order the
    /// claims were given.
    pub fn into_matches(self) -> Vec<Vec<Match>> {
        self.matches
            .into_iter()
            .map(|matches| matches.into_iter().collect())
            .collect()
    }
}

/// The byte offset each line of a text starts at.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(offset, _)| offset + 1),
        )
        .collect()
}

/// A claim's re-check (§9.3): its matches outside every accepted region
/// and excepted file. The claim holds when there are none.
///
/// A match lies in a region when every line it touches is strictly
/// between the region's markers. A file with a scan error contributes
/// no regions, so its matches lie outside.
pub fn outside(claim: &CompletenessClaim, matches: &[Match], accepted: &[&Region]) -> Vec<Match> {
    matches
        .iter()
        .filter(|found| {
            let excepted = claim
                .exceptions
                .iter()
                .any(|exception| exception.path == found.path);

            let inside = accepted.iter().any(|region| {
                let [begin, end] = region.lines;
                let [first, last] = found.lines;

                region.path == found.path && begin < first && last < end
            });

            !excepted && !inside
        })
        .cloned()
        .collect()
}
