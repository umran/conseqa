//! Choosing a realization (§10 of the realization profiles revision).
//!
//! A realizable declaration's **candidates** are the platform entries
//! realizing its kind whose technology the stack names at a version
//! inside the entry's range. A candidate with a set of its options is
//! **valid** when what it provides with them covers every need. Each
//! declaration is then in one state:
//!
//! - **chosen**: the architect's choice file names a valid entry and
//!   option set;
//! - **automatic**: no choice file, and exactly one valid candidate
//!   with exactly one minimal option set. Automatic choice never picks
//!   an option the declaration does not need, so a Kafka topic that
//!   needs no global order is never made single-partition by default;
//! - **ambiguous**: several valid candidates or option sets, so the
//!   architect chooses;
//! - **invalid choice**: a choice file whose entry no longer holds,
//!   since the needs grew, the stack changed, or the entry did. It is
//!   infeasible until re-chosen;
//! - **infeasible**: no valid candidate, reported with every candidate,
//!   the needs it misses, and why it cannot provide them (§10.4);
//! - **unneeded**: no choice file, and nothing needed, so nothing is
//!   chosen: a router without routing, a pool whose concurrency is not
//!   bounded, a topic runtime declaring nothing.
//!
//! Everything is recomputed on every derivation, so a spec change
//! adding a need the chosen realization lacks surfaces at the spec
//! gate, before any agent starts (§2.3).
//!
//! A choice file lives at `.conseqa/realization/choices/<kind>/<id>[/<id>].yaml`,
//! by the path rule of Part IV's records, and is written atomically
//! under a local lock. The store never loads or scans it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::analyzer::basis::TextualKey;
use crate::design::store::PROJECT_DIR;
use crate::spec::{Id, Model};

use super::capability::{self, Capability, Need, RealizableKind, realized_by};
use super::catalogue::{
    Catalogue, Catalogued, Entry, EntryRef, PlatformEntry, Source, entry_id, entry_ref,
    standard_name,
};
use super::stack::{self, Stack};
use super::{
    EntryKind, REALIZATION_FORMAT, RealizationError, check_format, entries, read_versioned,
    write_atomic,
};

/// The choices' directory, in the realization directory.
pub const DIR: &str = "choices";

/// The lock a choice is written under, in `.conseqa/`.
const LOCK: &str = "local/realization.lock";

/// The architect's choice for one declaration (§10.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub format: u32,
    pub declaration: TextualKey,
    pub entry: EntryRef,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A choice and the file holding it.
#[derive(Debug, Clone)]
pub struct ChoiceFile {
    pub path: PathBuf,
    pub choice: Choice,
}

/// Everything `.conseqa/realization/` holds, read strictly.
#[derive(Debug, Clone)]
pub struct Profile {
    pub stack: Stack,
    pub catalogue: Catalogue,
    pub choices: BTreeMap<TextualKey, ChoiceFile>,
}

impl Profile {
    /// The project's realization profile, or `None` when it has no
    /// stack profile, so nothing of realization applies (§9).
    pub fn read(project_dir: &Path) -> Result<Option<Self>, RealizationError> {
        let dir = super::dir(project_dir);

        for (name, kind) in entries(&dir)?.unwrap_or_default() {
            let expected = match name.as_str() {
                stack::FILE => EntryKind::File,
                super::catalogue::DIR | DIR => EntryKind::Directory,
                _ => EntryKind::Other,
            };

            if kind != expected || expected == EntryKind::Other {
                return Err(RealizationError::Unexpected {
                    path: dir.join(&name),
                    expected: format!(
                        "it holds `{}`, `{}/`, and `{DIR}/`",
                        stack::FILE,
                        super::catalogue::DIR
                    ),
                });
            }
        }

        let Some(stack) = Stack::read(project_dir)? else {
            return Ok(None);
        };

        let catalogue = Catalogue::read(project_dir)?;
        let mut choices = BTreeMap::new();

        let choices_dir = dir.join(DIR);

        for (name, kind) in entries(&choices_dir)?.unwrap_or_default() {
            let path = choices_dir.join(&name);

            let Some(realizable) =
                RealizableKind::named(&name).filter(|_| kind == EntryKind::Directory)
            else {
                return Err(RealizationError::Unexpected {
                    path,
                    expected: format!(
                        "it holds one directory per realizable kind: {}",
                        RealizableKind::ALL
                            .iter()
                            .map(|kind| format!("{kind}/"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            };

            read_level(realizable, &path, &mut Vec::new(), &mut choices)?;
        }

        Ok(Some(Self {
            stack,
            catalogue,
            choices,
        }))
    }

    /// Whether the stack names the entry's technology at a version in
    /// its range.
    fn in_stack(&self, entry: &PlatformEntry) -> bool {
        self.stack
            .version(&entry.technology.name)
            .is_some_and(|version| entry.technology.versions.contains(version))
    }

    /// A declaration's candidates (§10.2), by id.
    fn candidates(&self, kind: RealizableKind) -> impl Iterator<Item = (&PlatformEntry, Source)> {
        self.catalogue
            .platforms(kind)
            .filter(|(entry, _)| self.in_stack(entry))
    }
}

/// Reads one level of a kind's choices: directories named by the
/// containing declarations' ids, then `<id>.yaml` choices.
fn read_level(
    kind: RealizableKind,
    dir: &Path,
    ids: &mut Vec<Id>,
    choices: &mut BTreeMap<TextualKey, ChoiceFile>,
) -> Result<(), RealizationError> {
    let last = ids.len() + 1 == kind.arity();

    for (name, entry) in entries(dir)?.unwrap_or_default() {
        let path = dir.join(&name);

        let unexpected = || RealizationError::Unexpected {
            path: path.clone(),
            expected: if last {
                format!(
                    "a `{kind}` choice is a `<id>.yaml` file, named by the id's canonical encoding"
                )
            } else {
                format!(
                    "a directory of `{kind}` choices is named by the containing declaration's id, \
                     in its canonical encoding"
                )
            },
        };

        if last {
            let id = (entry == EntryKind::File)
                .then(|| name.strip_suffix(".yaml").and_then(Id::decode))
                .flatten()
                .ok_or_else(unexpected)?;

            ids.push(id);

            let key: TextualKey = format!(
                "{kind} {}",
                ids.iter().map(Id::encoded).collect::<Vec<_>>().join("/")
            )
            .parse()
            .expect("a realizable kind and its ids in canonical encoding are a key");

            ids.pop();

            let choice = read_choice(&path, &key)?;

            choices.insert(key, ChoiceFile { path, choice });
        } else {
            let id = (entry == EntryKind::Directory)
                .then(|| Id::decode(&name))
                .flatten()
                .ok_or_else(unexpected)?;

            ids.push(id);
            read_level(kind, &path, ids, choices)?;
            ids.pop();
        }
    }

    Ok(())
}

/// Reads the choice of `key` at `path`, strictly.
fn read_choice(path: &Path, key: &TextualKey) -> Result<Choice, RealizationError> {
    let text = read_versioned(path)?;

    let choice: Choice = serde_yaml::from_str(&text)
        .map_err(|error| RealizationError::parse(path, error.to_string()))?;

    if choice.declaration != *key {
        return Err(RealizationError::parse(
            path,
            format!(
                "it declares `{}`, but its path names `{key}`",
                choice.declaration
            ),
        ));
    }

    if entry_ref(&choice.entry.to_string()).is_none() {
        return Err(RealizationError::parse(
            path,
            format!(
                "`{}` is not an entry at a version, such as `kafka.keyed-topic@1`",
                choice.entry
            ),
        ));
    }

    let mut seen = BTreeSet::new();

    for option in &choice.options {
        if !seen.insert(option) {
            return Err(RealizationError::parse(
                path,
                format!("it names the option `{option}` twice"),
            ));
        }
    }

    Ok(choice)
}

/// Where a declaration's choice lives (§10.1): its kind as a directory,
/// then each escaped id as a path segment, the last with `.yaml`.
pub fn path(project_dir: &Path, key: &TextualKey) -> PathBuf {
    let declaration = key.key();
    let ids = declaration.ids().expect("a textual key names ids");
    let (last, containing) = ids.split_last().expect("a textual key names an id");

    let mut path = super::dir(project_dir).join(DIR).join(declaration.kind());

    for id in containing {
        path.push(id.encoded());
    }

    path.push(format!("{}.yaml", last.encoded()));

    path
}

/// A need a candidate does not cover, and why it cannot, if it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Missing {
    pub capability: Capability,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Missing {
    fn of(entry: &PlatformEntry, capability: Capability) -> Self {
        Self {
            capability,
            reason: entry
                .exclusion(capability)
                .map(|exclusion| exclusion.reason.clone()),
        }
    }
}

/// A valid candidate with one of its minimal option sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Valid {
    pub entry: EntryRef,
    pub options: Vec<String>,
}

/// What a candidate of an infeasible declaration misses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub entry: EntryRef,

    /// The needs it provides with no option set.
    pub missing: Vec<Missing>,

    /// It provides every need, but only with options that exclude one
    /// another.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub options_conflict: bool,
}

/// Why a choice does not hold, or cannot be made (§10.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum Problem {
    /// No catalogue holds the entry.
    UnknownEntry { entry: String },

    /// The entry is an idiom, which realizes nothing.
    NotPlatform { entry: String },

    /// The catalogue holds another version of the entry, whose claims
    /// may differ from those the choice was made against.
    OtherVersion {
        entry: String,
        chosen: u32,
        current: u32,
    },

    /// The entry realizes another kind.
    OtherKind {
        entry: String,
        realizes: RealizableKind,
        declaration: RealizableKind,
    },

    /// The stack does not name the entry's technology.
    NotInStack {
        technology: String,
        versions: String,
    },

    /// The stack names the technology at a version outside the entry's
    /// range.
    VersionOutsideRange {
        technology: String,
        versions: String,
        stack: String,
    },

    /// The entry defines no such option.
    UnknownOption {
        option: String,
        options: Vec<String>,
    },

    /// Two options that exclude each other.
    ExcludedOptions { option: String, excludes: String },

    /// What the entry provides with the options leaves needs uncovered.
    Uncovered { missing: Vec<Missing> },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEntry { entry } => write!(f, "no catalogue holds `{entry}`"),
            Self::NotPlatform { entry } => {
                write!(f, "`{entry}` is an idiom entry, which realizes nothing")
            }
            Self::OtherVersion {
                entry,
                chosen,
                current,
            } => write!(
                f,
                "`{entry}` is at version {current} now, and the choice names version {chosen}, \
                 whose claims may differ"
            ),
            Self::OtherKind {
                entry,
                realizes,
                declaration,
            } => write!(
                f,
                "`{entry}` realizes a `{realizes}`, and the declaration is a `{declaration}`"
            ),
            Self::NotInStack {
                technology,
                versions,
            } => write!(
                f,
                "the stack does not name `{technology}`, which the entry needs at {versions}"
            ),
            Self::VersionOutsideRange {
                technology,
                versions,
                stack,
            } => write!(
                f,
                "the stack names `{technology}@{stack}`, outside the entry's range {versions}"
            ),
            Self::UnknownOption { option, options } => {
                if options.is_empty() {
                    write!(f, "the entry defines no options, so not `{option}`")
                } else {
                    write!(
                        f,
                        "the entry defines no option `{option}`; its options are {}",
                        options.join(", ")
                    )
                }
            }
            Self::ExcludedOptions { option, excludes } => {
                write!(f, "the option `{option}` excludes `{excludes}`")
            }
            Self::Uncovered { missing } => {
                write!(f, "it does not provide ")?;

                for (index, missing) in missing.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }

                    write!(f, "`{}`", missing.capability)?;

                    if let Some(reason) = &missing.reason {
                        write!(f, " ({})", reason.trim())?;
                    }
                }

                Ok(())
            }
        }
    }
}

/// A declaration's state (§10.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// The architect's choice, and it holds.
    Chosen {
        entry: EntryRef,
        options: Vec<String>,

        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// The one valid candidate, with its one minimal option set.
    Automatic {
        entry: EntryRef,
        options: Vec<String>,
    },

    /// Every valid candidate with each of its minimal option sets: a
    /// choice is needed.
    Ambiguous { valid: Vec<Valid> },

    /// The architect's choice, and it no longer holds.
    InvalidChoice {
        entry: EntryRef,
        options: Vec<String>,
        problems: Vec<Problem>,
    },

    /// No candidate is valid: what each misses (§10.4).
    Infeasible { gaps: Vec<Gap> },

    /// Nothing is needed, and nothing was chosen.
    Unneeded,
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Chosen { .. } => "chosen",
            Self::Automatic { .. } => "automatic",
            Self::Ambiguous { .. } => "ambiguous",
            Self::InvalidChoice { .. } => "invalid_choice",
            Self::Infeasible { .. } => "infeasible",
            Self::Unneeded => "unneeded",
        }
    }

    /// Whether the state fails `--require-realizable` (§15.1).
    pub fn fails(&self) -> bool {
        matches!(
            self,
            Self::Ambiguous { .. } | Self::InvalidChoice { .. } | Self::Infeasible { .. }
        )
    }

    /// The entry that realizes the declaration, if one does.
    pub fn realization(&self) -> Option<(&EntryRef, &[String])> {
        match self {
            Self::Chosen { entry, options, .. } | Self::Automatic { entry, options } => {
                Some((entry, options))
            }
            _ => None,
        }
    }
}

/// One realizable declaration: what it needs, and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Realized {
    pub declaration: TextualKey,
    pub needs: Vec<Need>,

    #[serde(flatten)]
    pub state: State,
}

/// A choice whose declaration is not in the spec, or not realizable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Orphaned {
    pub declaration: TextualKey,
    pub entry: EntryRef,
}

/// The stack and catalogue a status was judged with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileView {
    pub catalogue: String,
    pub technologies: Vec<String>,
}

/// Every realizable declaration's state (§15.1's `status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// The stack and catalogue, or `None` without a stack profile, when
    /// nothing is realized (§9).
    pub profile: Option<ProfileView>,

    pub declarations: Vec<Realized>,
    pub orphaned: Vec<Orphaned>,
}

impl Status {
    /// The project's realization status, or a status with no profile
    /// when it has no stack.
    pub fn derive(project_dir: &Path, model: &Model) -> Result<Self, RealizationError> {
        Ok(match Profile::read(project_dir)? {
            Some(profile) => Self::of(&profile, model),
            None => Self {
                profile: None,
                declarations: Vec::new(),
                orphaned: Vec::new(),
            },
        })
    }

    /// The status of a model under a profile.
    pub fn of(profile: &Profile, model: &Model) -> Self {
        let needs = capability::needs(model);

        let orphaned = profile
            .choices
            .iter()
            .filter(|(key, _)| !needs.contains_key(*key))
            .map(|(key, file)| Orphaned {
                declaration: key.clone(),
                entry: file.choice.entry.clone(),
            })
            .collect();

        let declarations = needs
            .into_iter()
            .map(|(declaration, needs)| {
                let state = state_of(profile, &declaration, &needs);

                Realized {
                    declaration,
                    needs,
                    state,
                }
            })
            .collect();

        Self {
            profile: Some(ProfileView {
                catalogue: standard_name(),
                technologies: profile.stack.named(),
            }),
            declarations,
            orphaned,
        }
    }

    /// The declarations `--require-realizable` fails on.
    pub fn failing(&self) -> impl Iterator<Item = &Realized> {
        self.declarations
            .iter()
            .filter(|realized| realized.state.fails())
    }

    /// The status as text: one line per declaration, then what makes
    /// each failing one fail.
    pub fn render(&self) -> String {
        let Some(profile) = &self.profile else {
            return "no stack profile at .conseqa/realization/stack.yaml; realization does not \
                    apply\n"
                .to_string();
        };

        let mut text = format!(
            "realization against {}, with {}\n",
            profile.catalogue,
            if profile.technologies.is_empty() {
                "no technologies".to_string()
            } else {
                profile.technologies.join(", ")
            }
        );

        let width = self
            .declarations
            .iter()
            .map(|realized| realized.declaration.to_string().len())
            .max()
            .unwrap_or(0);

        for realized in &self.declarations {
            let detail = match &realized.state {
                State::Chosen { entry, options, .. } | State::Automatic { entry, options } => {
                    with_options(entry, options)
                }
                State::Ambiguous { valid } => valid
                    .iter()
                    .map(|valid| with_options(&valid.entry, &valid.options))
                    .collect::<Vec<_>>()
                    .join(" | "),
                State::InvalidChoice { entry, options, .. } => with_options(entry, options),
                State::Infeasible { .. } | State::Unneeded => String::new(),
            };

            text.push_str(
                format!(
                    "{:<14} {:<width$}  {detail}",
                    realized.state.name(),
                    realized.declaration.to_string()
                )
                .trim_end(),
            );
            text.push('\n');

            match &realized.state {
                State::InvalidChoice { problems, .. } => {
                    for problem in problems {
                        text.push_str(&format!("  {problem}\n"));
                    }
                }
                State::Infeasible { gaps } => {
                    text.push_str(&infeasibility(&realized.needs, gaps));
                }
                _ => {}
            }
        }

        for orphaned in &self.orphaned {
            text.push_str(&format!(
                "orphaned       {}  {}: the spec has no such realizable declaration\n",
                orphaned.declaration, orphaned.entry
            ));
        }

        let count = |name: &str| {
            self.declarations
                .iter()
                .filter(|realized| realized.state.name() == name)
                .count()
        };

        text.push_str(&format!(
            "{} chosen, {} automatic, {} ambiguous, {} invalid choice, {} infeasible, {} unneeded\n",
            count("chosen"),
            count("automatic"),
            count("ambiguous"),
            count("invalid_choice"),
            count("infeasible"),
            count("unneeded"),
        ));

        text
    }
}

fn with_options(entry: &EntryRef, options: &[String]) -> String {
    if options.is_empty() {
        entry.to_string()
    } else {
        format!("{entry} [{}]", options.join(", "))
    }
}

/// An infeasible declaration's report (§10.4): each missing need with
/// where the spec asks for it, then each candidate with what it cannot
/// provide and why.
fn infeasibility(needs: &[Need], gaps: &[Gap]) -> String {
    let mut text = String::new();

    let missing: BTreeSet<Capability> = gaps
        .iter()
        .flat_map(|gap| gap.missing.iter().map(|missing| missing.capability))
        .collect();

    for need in needs {
        if gaps.is_empty() || missing.contains(&need.capability) {
            for source in &need.sources {
                text.push_str(&format!("  needs {}    {source}\n", need.capability));
            }
        }
    }

    if gaps.is_empty() {
        text.push_str("  no entry in the stack realizes this kind\n");
    }

    for gap in gaps {
        for missing in &gap.missing {
            match &missing.reason {
                Some(reason) => text.push_str(&format!(
                    "  {}  cannot provide {}: {}\n",
                    gap.entry,
                    missing.capability,
                    reason.trim()
                )),
                None => text.push_str(&format!(
                    "  {}  does not provide {}\n",
                    gap.entry, missing.capability
                )),
            }
        }

        if gap.options_conflict {
            text.push_str(&format!(
                "  {}  provides every need only with options that exclude one another\n",
                gap.entry
            ));
        }
    }

    text
}

/// What one candidate provides of a set of needs.
struct Judged<'e> {
    entry: &'e PlatformEntry,

    /// Every minimal option set it is valid with, smallest first.
    minimal: Vec<Vec<String>>,

    /// The needs no option set provides.
    missing: Vec<Capability>,
}

/// Judges a candidate against needs: every option set it is valid
/// with, and the minimal ones, from which no option can be dropped
/// (§10.3). An entry defines at most sixteen options, so every set can
/// be tried.
fn judge<'e>(entry: &'e PlatformEntry, needs: &BTreeSet<Capability>) -> Judged<'e> {
    let options: Vec<&String> = entry.options.keys().collect();

    let set = |mask: u32| -> BTreeSet<String> {
        options
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, option)| (*option).clone())
            .collect()
    };

    let valid: BTreeSet<u32> = (0..1u32 << options.len())
        .filter(|mask| {
            let options = set(*mask);

            entry.conflict(&options).is_none() && needs.is_subset(&entry.provides_with(&options))
        })
        .collect();

    let mut minimal: Vec<Vec<String>> = valid
        .iter()
        .filter(|mask| {
            (0..options.len())
                .all(|bit| *mask & (1 << bit) == 0 || !valid.contains(&(*mask & !(1 << bit))))
        })
        .map(|mask| set(*mask).into_iter().collect())
        .collect();

    minimal.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));

    let everything = entry.provides_with(&entry.options.keys().cloned().collect());

    Judged {
        entry,
        minimal,
        missing: needs.difference(&everything).copied().collect(),
    }
}

/// The state of one declaration under a profile.
fn state_of(profile: &Profile, declaration: &TextualKey, needs: &[Need]) -> State {
    let kind = RealizableKind::of(declaration.key()).expect("needs are of realizable declarations");
    let needed: BTreeSet<Capability> = needs.iter().map(|need| need.capability).collect();

    if let Some(file) = profile.choices.get(declaration) {
        let choice = &file.choice;

        return match validate(profile, kind, &needed, &choice.entry, &choice.options) {
            Ok(()) => State::Chosen {
                entry: choice.entry.clone(),
                options: sorted(&choice.options),
                reason: choice.reason.clone(),
            },
            Err(problems) => State::InvalidChoice {
                entry: choice.entry.clone(),
                options: sorted(&choice.options),
                problems,
            },
        };
    }

    if needed.is_empty() {
        return State::Unneeded;
    }

    let judged: Vec<Judged<'_>> = profile
        .candidates(kind)
        .map(|(entry, _)| judge(entry, &needed))
        .collect();

    let mut valid: Vec<Valid> = judged
        .iter()
        .flat_map(|judged| {
            judged.minimal.iter().map(|options| Valid {
                entry: judged.entry.entry_ref(),
                options: options.clone(),
            })
        })
        .collect();

    match valid.len() {
        0 => State::Infeasible {
            gaps: judged
                .iter()
                .map(|judged| Gap {
                    entry: judged.entry.entry_ref(),
                    missing: judged
                        .missing
                        .iter()
                        .map(|capability| Missing::of(judged.entry, *capability))
                        .collect(),
                    options_conflict: judged.missing.is_empty(),
                })
                .collect(),
        },

        1 => {
            let valid = valid.remove(0);

            State::Automatic {
                entry: valid.entry,
                options: valid.options,
            }
        }

        _ => State::Ambiguous { valid },
    }
}

fn sorted(options: &[String]) -> Vec<String> {
    let mut options = options.to_vec();
    options.sort();
    options
}

/// Why an entry and option set are not a valid realization of a
/// declaration of `kind` with these needs, or nothing when they are.
fn validate(
    profile: &Profile,
    kind: RealizableKind,
    needed: &BTreeSet<Capability>,
    chosen: &EntryRef,
    options: &[String],
) -> Result<(), Vec<Problem>> {
    let entry = match profile.catalogue.get(&chosen.id) {
        None => {
            return Err(vec![Problem::UnknownEntry {
                entry: chosen.id.clone(),
            }]);
        }

        Some(Catalogued {
            entry: Entry::Idiom(_),
            ..
        }) => {
            return Err(vec![Problem::NotPlatform {
                entry: chosen.id.clone(),
            }]);
        }

        Some(Catalogued {
            entry: Entry::Platform(entry),
            ..
        }) => entry,
    };

    if entry.version != chosen.version {
        return Err(vec![Problem::OtherVersion {
            entry: chosen.id.clone(),
            chosen: chosen.version,
            current: entry.version,
        }]);
    }

    if entry.kind() != kind {
        return Err(vec![Problem::OtherKind {
            entry: chosen.id.clone(),
            realizes: entry.kind(),
            declaration: kind,
        }]);
    }

    let mut problems = Vec::new();

    let technology = &entry.technology;

    match profile.stack.version(&technology.name) {
        None => problems.push(Problem::NotInStack {
            technology: technology.name.clone(),
            versions: technology.versions.to_string(),
        }),
        Some(version) if !technology.versions.contains(version) => {
            problems.push(Problem::VersionOutsideRange {
                technology: technology.name.clone(),
                versions: technology.versions.to_string(),
                stack: version.to_string(),
            });
        }
        Some(_) => {}
    }

    let mut known = true;

    for option in options {
        if !entry.options.contains_key(option) {
            known = false;

            problems.push(Problem::UnknownOption {
                option: option.clone(),
                options: entry.options.keys().cloned().collect(),
            });
        }
    }

    if known {
        let set: BTreeSet<String> = options.iter().cloned().collect();

        if let Some((option, excludes)) = entry.conflict(&set) {
            problems.push(Problem::ExcludedOptions {
                option: option.to_string(),
                excludes: excludes.to_string(),
            });
        }

        let provided = entry.provides_with(&set);
        let missing: Vec<Missing> = needed
            .difference(&provided)
            .map(|capability| Missing::of(entry, *capability))
            .collect();

        if !missing.is_empty() {
            problems.push(Problem::Uncovered { missing });
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// A candidate as `explain` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub entry: EntryRef,
    pub source: Source,
    pub provides: Vec<Offer>,
    pub missing: Vec<Missing>,

    /// Every minimal option set it is valid with; none when it is not
    /// valid.
    pub option_sets: Vec<Vec<String>>,
}

/// A capability a candidate provides, with the option it needs, if one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub capability: Capability,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub with: Option<String>,
}

/// An entry of the kind outside the stack, and why: what changing the
/// stack would offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outside {
    pub entry: EntryRef,
    pub source: Source,
    pub technology: String,
    pub versions: String,

    /// The version the stack names, when it names the technology at a
    /// version outside the range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

/// Every candidate of one declaration: what each provides, what it
/// misses, and why (§15.1's `explain`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    pub declaration: TextualKey,
    pub needs: Vec<Need>,

    #[serde(flatten)]
    pub state: State,

    pub candidates: Vec<Candidate>,
    pub outside: Vec<Outside>,
}

/// Why a declaration cannot be explained or chosen for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    /// The project has no stack profile, so nothing is realized.
    NoStack,

    /// The spec has no such declaration.
    UnknownDeclaration { declaration: String },

    /// The declaration's kind is not realizable; a data object names
    /// the data model that realizes it.
    NotRealizable {
        declaration: String,

        #[serde(skip_serializing_if = "Option::is_none")]
        realized_by: Option<String>,
    },

    /// The entry is neither an id nor an id at a version.
    MalformedEntry { entry: String },

    /// An option named twice.
    DuplicateOption { option: String },

    /// The entry and options are not a valid realization.
    Invalid {
        entry: String,
        problems: Vec<Problem>,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStack => f.write_str(
                "the project has no stack profile, so nothing is realized; write \
                 .conseqa/realization/stack.yaml first",
            ),
            Self::UnknownDeclaration { declaration } => {
                write!(f, "the spec has no declaration `{declaration}`")
            }
            Self::NotRealizable {
                declaration,
                realized_by,
            } => match realized_by {
                Some(realized_by) => write!(
                    f,
                    "`{declaration}` is realized by its data model's store: see `{realized_by}`"
                ),
                None => write!(
                    f,
                    "`{declaration}` is not of a realizable kind; the realizable kinds are {}",
                    RealizableKind::ALL
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
            Self::MalformedEntry { entry } => write!(
                f,
                "`{entry}` is not an entry: write its id, such as `kafka.keyed-topic`, or its id \
                 at a version, such as `kafka.keyed-topic@1`"
            ),
            Self::DuplicateOption { option } => write!(f, "the option `{option}` is named twice"),
            Self::Invalid { entry, problems } => {
                write!(f, "`{entry}` is not a valid realization: ")?;

                for (index, problem) in problems.iter().enumerate() {
                    if index > 0 {
                        f.write_str("; ")?;
                    }

                    write!(f, "{problem}")?;
                }

                Ok(())
            }
        }
    }
}

/// The needs of a declaration the spec realizes, or why there are
/// none.
fn needs_of(model: &Model, declaration: &TextualKey) -> Result<Vec<Need>, Refusal> {
    if RealizableKind::of(declaration.key()).is_none() {
        return Err(Refusal::NotRealizable {
            declaration: declaration.to_string(),
            realized_by: realized_by(declaration.key()).map(|key| key.to_string()),
        });
    }

    capability::needs(model)
        .remove(declaration)
        .ok_or_else(|| Refusal::UnknownDeclaration {
            declaration: declaration.to_string(),
        })
}

impl Explanation {
    /// Every candidate of a declaration, and the kind's entries outside
    /// the stack.
    pub fn of(profile: &Profile, model: &Model, declaration: &TextualKey) -> Result<Self, Refusal> {
        let needs = needs_of(model, declaration)?;
        let kind = RealizableKind::of(declaration.key()).expect("checked by needs_of");
        let needed: BTreeSet<Capability> = needs.iter().map(|need| need.capability).collect();

        let state = state_of(profile, declaration, &needs);

        let mut candidates = Vec::new();
        let mut outside = Vec::new();

        for (entry, source) in profile.catalogue.platforms(kind) {
            if profile.in_stack(entry) {
                let judged = judge(entry, &needed);

                candidates.push(Candidate {
                    entry: entry.entry_ref(),
                    source,
                    provides: entry
                        .provides
                        .iter()
                        .map(|provision| Offer {
                            capability: provision.capability,
                            with: provision.with.as_ref().map(|with| with.option.clone()),
                        })
                        .collect(),
                    missing: judged
                        .missing
                        .iter()
                        .map(|capability| Missing::of(entry, *capability))
                        .collect(),
                    option_sets: judged.minimal,
                });
            } else {
                outside.push(Outside {
                    entry: entry.entry_ref(),
                    source,
                    technology: entry.technology.name.clone(),
                    versions: entry.technology.versions.to_string(),
                    stack: profile
                        .stack
                        .version(&entry.technology.name)
                        .map(ToString::to_string),
                });
            }
        }

        Ok(Self {
            declaration: declaration.clone(),
            needs,
            state,
            candidates,
            outside,
        })
    }

    pub fn render(&self) -> String {
        let mut text = format!("{}  {}\n", self.state.name(), self.declaration);

        if self.needs.is_empty() {
            text.push_str("needs nothing\n");
        }

        for need in &self.needs {
            text.push_str(&format!("needs {}\n", need.capability));

            for source in &need.sources {
                text.push_str(&format!("  {source}\n"));
            }
        }

        if let State::InvalidChoice { problems, .. } = &self.state {
            for problem in problems {
                text.push_str(&format!("invalid choice: {problem}\n"));
            }
        }

        if self.candidates.is_empty() {
            text.push_str("no entry in the stack realizes this kind\n");
        }

        for candidate in &self.candidates {
            text.push_str(&format!(
                "candidate {} ({})\n",
                candidate.entry, candidate.source
            ));

            let offers: Vec<String> = candidate
                .provides
                .iter()
                .map(|offer| match &offer.with {
                    Some(option) => format!("{} (with {option})", offer.capability),
                    None => offer.capability.to_string(),
                })
                .collect();

            text.push_str(&format!(
                "  provides {}\n",
                if offers.is_empty() {
                    "nothing".to_string()
                } else {
                    offers.join(", ")
                }
            ));

            for missing in &candidate.missing {
                match &missing.reason {
                    Some(reason) => text.push_str(&format!(
                        "  cannot provide {}: {}\n",
                        missing.capability,
                        reason.trim()
                    )),
                    None => text.push_str(&format!("  does not provide {}\n", missing.capability)),
                }
            }

            if candidate.option_sets.is_empty() {
                text.push_str("  not valid\n");
            }

            for options in &candidate.option_sets {
                text.push_str(&format!(
                    "  valid {}\n",
                    if options.is_empty() {
                        "with no options".to_string()
                    } else {
                        format!("with {}", options.join(", "))
                    }
                ));
            }
        }

        for outside in &self.outside {
            text.push_str(&match &outside.stack {
                Some(stack) => format!(
                    "outside the stack: {} needs {}{}, and the stack names {}@{stack}\n",
                    outside.entry, outside.technology, outside.versions, outside.technology
                ),
                None => format!(
                    "outside the stack: {} needs {} {}\n",
                    outside.entry, outside.technology, outside.versions
                ),
            });
        }

        text
    }
}

/// A choice to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChooseRequest {
    pub declaration: TextualKey,

    /// An entry id, or an id at a version.
    pub entry: String,

    pub options: Vec<String>,
    pub reason: Option<String>,
}

/// A choice written, or found already written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chosen {
    pub declaration: TextualKey,
    pub entry: EntryRef,
    pub options: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    /// The choice file, relative to the project.
    pub path: String,

    /// Whether the file was written: an identical choice writes
    /// nothing.
    pub written: bool,

    /// Whether it replaced another choice.
    pub replaced: bool,
}

impl Chosen {
    pub fn render(&self) -> String {
        let action = match (self.written, self.replaced) {
            (false, _) => "already chosen",
            (true, true) => "rechose",
            (true, false) => "chose",
        };

        format!(
            "{action} {} for {} ({})\n",
            with_options(&self.entry, &self.options),
            self.declaration,
            self.path
        )
    }
}

/// Why a choice was not written.
#[derive(Debug)]
pub enum ChooseError {
    Refused(Box<Refusal>),
    Failed(RealizationError),
}

impl From<RealizationError> for ChooseError {
    fn from(error: RealizationError) -> Self {
        Self::Failed(error)
    }
}

impl fmt::Display for ChooseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::Failed(error) => write!(f, "{error}"),
        }
    }
}

/// Writes a choice (§15.1's `choose`), refusing an invalid one by
/// naming what it lacks. An identical choice is left as it is.
pub fn choose(
    project_dir: &Path,
    model: &Model,
    request: &ChooseRequest,
) -> Result<Chosen, ChooseError> {
    let refused = |refusal: Refusal| ChooseError::Refused(Box::new(refusal));

    let _lock = lock(project_dir)?;

    let profile = Profile::read(project_dir)?.ok_or_else(|| refused(Refusal::NoStack))?;

    let needs = needs_of(model, &request.declaration).map_err(refused)?;
    let kind = RealizableKind::of(request.declaration.key()).expect("checked by needs_of");
    let needed: BTreeSet<Capability> = needs.iter().map(|need| need.capability).collect();

    let entry = if request.entry.contains('@') {
        entry_ref(&request.entry).ok_or_else(|| {
            refused(Refusal::MalformedEntry {
                entry: request.entry.clone(),
            })
        })?
    } else if entry_id(&request.entry) {
        let version = profile
            .catalogue
            .get(&request.entry)
            .map(|catalogued| catalogued.entry.version())
            .ok_or_else(|| {
                refused(Refusal::Invalid {
                    entry: request.entry.clone(),
                    problems: vec![Problem::UnknownEntry {
                        entry: request.entry.clone(),
                    }],
                })
            })?;

        EntryRef {
            id: request.entry.clone(),
            version,
        }
    } else {
        return Err(refused(Refusal::MalformedEntry {
            entry: request.entry.clone(),
        }));
    };

    let mut options = Vec::new();

    for option in &request.options {
        if options.contains(option) {
            return Err(refused(Refusal::DuplicateOption {
                option: option.clone(),
            }));
        }

        options.push(option.clone());
    }

    options.sort();

    validate(&profile, kind, &needed, &entry, &options).map_err(|problems| {
        refused(Refusal::Invalid {
            entry: entry.to_string(),
            problems,
        })
    })?;

    let choice = Choice {
        format: REALIZATION_FORMAT,
        declaration: request.declaration.clone(),
        entry: entry.clone(),
        options: options.clone(),
        reason: request
            .reason
            .as_ref()
            .map(|reason| reason.trim().to_string())
            .filter(|reason| !reason.is_empty()),
    };

    let path = path(project_dir, &request.declaration);
    let existing = profile.choices.get(&request.declaration);

    let written = existing.is_none_or(|file| file.choice != choice);

    if written {
        let text = serde_yaml::to_string(&choice).map_err(|error| {
            RealizationError::parse(&path, format!("the choice does not serialize: {error}"))
        })?;

        check_format(&path, &text)?;
        write_atomic(&path, text.as_bytes())?;
    }

    Ok(Chosen {
        declaration: request.declaration.clone(),
        entry,
        options,
        reason: choice.reason,
        path: relative(project_dir, &path),
        written,
        replaced: written && existing.is_some(),
    })
}

/// Takes the choice lock, waiting for another writer to finish.
fn lock(project_dir: &Path) -> Result<File, RealizationError> {
    let path = project_dir.join(PROJECT_DIR).join(LOCK);

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(RealizationError::io(dir))?;
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(RealizationError::io(&path))?;

    file.lock().map_err(RealizationError::io(&path))?;

    Ok(file)
}

fn relative(project_dir: &Path, path: &Path) -> String {
    path.strip_prefix(project_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
