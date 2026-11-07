//! Realization profiles: which technologies can realize a spec, and
//! which one does (the realization profiles revision,
//! `Conseqa_Realization_Profiles_Revision_Specification.md`).
//!
//! A proof is a proof of the model, conditional on the code and the
//! infrastructure doing what the model declares (§1.3 of the semantics
//! contract). Nothing in the model names a technology, and nothing may
//! (§10.9 of the semantics contract), so nothing until now told anyone
//! that `isolation: serializable` on Oracle is snapshot isolation, or
//! that an SQS standard queue orders nothing. This module says so once,
//! beside the spec:
//!
//! - [`capability`] is the closed vocabulary of what a realization can
//!   provide, in the semantics contract's own terms, and derives what
//!   each declaration needs from the spec, which declares nothing of it
//!   twice (§6);
//! - [`catalogue`] reads catalogue entries: what one technology,
//!   configured a given way, provides, cannot provide, and requires,
//!   with the vendor documentation each claim rests on (§7, §8). The
//!   standard catalogue is built into the binary; a project adds its
//!   own;
//! - [`stack`] reads the technologies one project uses, at their
//!   versions (§9);
//! - [`choice`] judges each declaration's candidates against its needs:
//!   chosen by the architect, automatic when exactly one fits,
//!   ambiguous, an invalid choice, infeasible, or unneeded (§10);
//! - [`requirements`] resolves what each chosen entry demands, of the
//!   declaration it realizes and of that declaration's neighbours, into
//!   the `realization` facts Part IV requires of the code (§11);
//! - [`checkers`] are the deterministic checks an entry configures for
//!   the evidence of some requirements, over the regions bound to the
//!   declaration a requirement is on (§13);
//! - [`document`] is the JSON each `conseqa realize` subcommand prints,
//!   and each realization tool of the design MCP server returns.
//!
//! The profile lives in `.conseqa/realization/`, beside the spec but
//! outside what the store loads and scans (§10.1). Without a stack
//! profile none of this applies (§9). Feasibility never changes a
//! verdict: a spec is proven, realizable, and conformant separately,
//! and each failure points somewhere different (§16).

use std::fs;
use std::path::{Path, PathBuf};

use crate::design::store::{FileSource, PROJECT_DIR, SourceEntryKind, StoreError};

pub mod capability;
pub mod catalogue;
pub mod checkers;
pub mod choice;
pub mod document;
pub mod requirements;
pub mod stack;

/// The realization format (§14): of the stack profile, choice files,
/// catalogue entries, and the capability vocabulary together. A DSL
/// bump forces a bump here, since capabilities are stated in the DSL's
/// terms; never the reverse.
pub const REALIZATION_FORMAT: u32 = 1;

/// The directory, inside `.conseqa/`, that holds the profile.
pub const DIR: &str = "realization";

/// The project's realization directory.
pub fn dir(project_dir: &Path) -> PathBuf {
    project_dir.join(PROJECT_DIR).join(DIR)
}

/// Why the realization profile could not be read, or a choice not
/// written. Each names the file that failed. A profile that meets one
/// fails as a whole, as the store fails on a broken spec file.
#[derive(Debug, thiserror::Error)]
pub enum RealizationError {
    #[error("{}: {error}", .path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },

    /// An entry the realization directory may not hold (§10.1).
    #[error("{}: unexpected entry; {expected}", .path.display())]
    Unexpected { path: PathBuf, expected: String },

    /// A realization file does not parse, or breaks a rule of its
    /// format (§8.2).
    #[error("{} does not load: {message}", .path.display())]
    Parse { path: PathBuf, message: String },

    /// A realization file of another format (§14).
    #[error(
        "{} is realization format {found}, but this build reads format {REALIZATION_FORMAT}",
        .path.display()
    )]
    FormatMismatch { path: PathBuf, found: u64 },

    /// The stack names another standard catalogue than the one this
    /// build ships. Adopting a new one is a change to the stack, made
    /// through the spec's pull request like any other.
    #[error(
        "{} names the catalogue `{found}`, but this build ships `{}`; review what changed \
         in the standard catalogue, then name it in the stack",
        .path.display(),
        catalogue::standard_name()
    )]
    CatalogueMismatch { path: PathBuf, found: String },

    /// A project entry with a standard entry's id (§8.1).
    #[error(
        "{}: `{id}` is a standard entry's id; a project that disagrees with a standard entry \
         copies it under an id of its own and says why",
        .path.display()
    )]
    DuplicateEntry { path: PathBuf, id: String },

    /// The spec does not assemble into a model, or the model does not
    /// validate, so it has no needs to judge.
    #[error("the spec cannot be realized: {reason}")]
    Unloadable { reason: String },

    /// A file source could not be read: a directory or a git tree.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl RealizationError {
    fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();

        move |error| Self::Io { path, error }
    }

    fn parse(path: &Path, message: impl Into<String>) -> Self {
        Self::Parse {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,

    /// A symbolic link, or anything else no reader opens.
    Other,
}

/// The realization files of one source, read through the store's file
/// sources: a project's own directory, or the tree a diff side reads
/// from a commit (Part I §13.4), so both are read under the same rules.
/// Paths are relative to the realization directory.
pub(crate) struct Files<'a> {
    source: &'a dyn FileSource,
}

impl<'a> Files<'a> {
    pub(crate) fn new(source: &'a dyn FileSource) -> Self {
        Self { source }
    }

    fn relative(path: &str) -> String {
        if path.is_empty() {
            DIR.to_string()
        } else {
            format!("{DIR}/{path}")
        }
    }

    /// How a refusal names `path`.
    pub(crate) fn locate(&self, path: &str) -> PathBuf {
        self.source.locate(&Self::relative(path))
    }

    /// The file's text, or `None` when there is none.
    pub(crate) fn read(&self, path: &str) -> Result<Option<String>, RealizationError> {
        Ok(self.source.read(&Self::relative(path))?)
    }

    /// The file's text, its `format` checked first, leniently, so that
    /// a file of another format is refused by its format rather than for
    /// a field this build's format does not share (§14).
    pub(crate) fn read_versioned(&self, path: &str) -> Result<Option<String>, RealizationError> {
        let Some(text) = self.read(path)? else {
            return Ok(None);
        };

        check_format(&self.locate(path), &text)?;

        Ok(Some(text))
    }

    /// A directory's entries, sorted by name, dotfiles left out: they are
    /// an editor's or the OS's, as the temporary of an atomic write is.
    /// `None` when there is no directory.
    fn entries(&self, path: &str) -> Result<Option<Vec<(String, EntryKind)>>, RealizationError> {
        let Some(entries) = self.source.entries(&Self::relative(path))? else {
            return Ok(None);
        };

        let mut entries: Vec<(String, EntryKind)> = entries
            .into_iter()
            .filter(|entry| !entry.name.starts_with('.'))
            .map(|entry| {
                let kind = match entry.kind {
                    SourceEntryKind::File => EntryKind::File,
                    SourceEntryKind::Directory => EntryKind::Directory,
                    SourceEntryKind::Other => EntryKind::Other,
                };

                (entry.name, kind)
            })
            .collect();

        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        Ok(Some(entries))
    }
}

/// Refuses a file whose `format` is missing, or another format.
fn check_format(path: &Path, text: &str) -> Result<(), RealizationError> {
    let probe: serde_yaml::Value = serde_yaml::from_str(text)
        .map_err(|error| RealizationError::parse(path, error.to_string()))?;

    let serde_yaml::Value::Mapping(mapping) = probe else {
        return Err(RealizationError::parse(path, "the file is not a mapping"));
    };

    match mapping.get("format") {
        None => Err(RealizationError::parse(path, "missing `format`")),

        Some(format) => match format.as_u64() {
            Some(found) if found == u64::from(REALIZATION_FORMAT) => Ok(()),
            Some(found) => Err(RealizationError::FormatMismatch {
                path: path.to_path_buf(),
                found,
            }),
            None => Err(RealizationError::parse(path, "`format` is not a number")),
        },
    }
}

/// Writes through a temporary sibling and a rename, so a reader sees
/// the old file or the new one, never part of either. The temporary is
/// a dotfile, which the reader passes over.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), RealizationError> {
    let dir = path.parent().unwrap_or(Path::new("."));

    fs::create_dir_all(dir).map_err(RealizationError::io(dir))?;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let temporary = dir.join(format!(".{name}.tmp-{}", std::process::id()));

    fs::write(&temporary, contents).map_err(RealizationError::io(&temporary))?;

    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);

        RealizationError::Io {
            path: path.to_path_buf(),
            error,
        }
    })
}
