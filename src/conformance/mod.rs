//! Conformance: what a repository's code was checked to provide, per
//! declaration (the conformance and implementation guidance revision,
//! `Conseqa_Conformance_and_Implementation_Guidance_Revision_Specification.md`).
//!
//! Every proof is conditional on the code conforming to the declarations
//! it consumed (§25 of the semantics contract), and nothing else connects
//! a declaration to code. This module does, without parsing any
//! language:
//!
//! - [`markers`] scans the repository's text files for binding markers,
//!   comment pairs that bind a region of a file to a declaration, and
//!   digests each region's own lines (§6);
//! - [`record`] reads and writes conformance records, one committed file
//!   per declaration stating what its regions were checked to provide
//!   (§8);
//! - [`policy`] reads which kinds must be bound and what needs a review
//!   (§14.4);
//! - [`completeness`] re-checks the claims that no unbound code touches
//!   what a model-wide fact depends on (§9);
//! - [`state`] derives each declaration's conditions and each proven
//!   obligation's conformance, from the spec, the files, the records,
//!   and the policy read at one moment (§10).
//!
//! Records and the policy live in `.conseqa/conformance/`, beside the
//! spec but outside what the store loads and scans (§8.1). Writing a
//! record therefore never adopts a spec revision, never invalidates a
//! design task, and never takes the spec writer lock.

use std::fmt;
use std::path::PathBuf;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::analyzer::basis::DeclarationKey;

pub mod completeness;
pub mod markers;
pub mod policy;
pub mod record;
pub mod state;

/// The conformance format, of records and of the policy file alike
/// (§8.2, §14.4). It counts every change to either: a field added,
/// removed, renamed, or retyped.
pub const FORMAT: u32 = 1;

/// The directory, inside `.conseqa/`, that holds the records and the
/// policy.
pub const DIR: &str = "conformance";

/// Whether declarations of a kind can be bound to code (§6.4): every
/// kind with a textual key except a prompt obligation, which is intent.
/// A requirement has no textual key at all; proofs discharge it, not
/// code.
pub fn bindable_kind(kind: &str) -> bool {
    kind != "prompt_obligation"
        && DeclarationKey::textual_kinds()
            .iter()
            .any(|(textual, _)| *textual == kind)
}

/// Whether a declaration can be bound to code (§6.4).
pub fn bindable(declaration: &DeclarationKey) -> bool {
    bindable_kind(declaration.kind())
}

/// The bindable kinds in §7.2's order, each with how many ids its key
/// names.
pub fn bindable_kinds() -> impl Iterator<Item = (&'static str, usize)> {
    DeclarationKey::textual_kinds()
        .iter()
        .copied()
        .filter(|(kind, _)| bindable_kind(kind))
}

/// A BLAKE3 hash, written as 64 lowercase hex digits: of a region's
/// residual (§6.3), or of a record file's bytes, which a write names to
/// replace it (§8.4).
///
/// Unlike the design layer's semantic hashes, digests are committed, in
/// records, so they are permanent: a digest hashes text the tool reads
/// from files, never a serialization the tool may change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn to_hex(&self) -> String {
        let mut hex = String::with_capacity(64);

        for byte in self.0 {
            use fmt::Write;

            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
        }

        hex
    }

    /// The digest `text` spells, if it is exactly 64 lowercase hex
    /// digits: one spelling per digest, as for every committed value.
    pub fn parse(text: &str) -> Option<Self> {
        let valid = text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));

        if !valid {
            return None;
        }

        let mut bytes = [0u8; 32];

        for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(pair).ok()?;

            bytes[index] = u8::from_str_radix(pair, 16).ok()?;
        }

        Some(Self(bytes))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(DigestVisitor)
    }
}

struct DigestVisitor;

impl Visitor<'_> for DigestVisitor {
    type Value = Digest;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a digest: 64 lowercase hex digits")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
        Digest::parse(text)
            .ok_or_else(|| E::custom(format!("`{text}` is not a digest: 64 lowercase hex digits")))
    }
}

/// Why the conformance inputs could not be read, or a record not
/// written. Each names the file or the command that failed. A
/// derivation that meets one fails as a whole, as the store fails on a
/// broken spec file: only a record of another version is set aside
/// instead (§8.6).
#[derive(Debug, thiserror::Error)]
pub enum ConformanceError {
    #[error("{}: {error}", .path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },

    #[error("git {command} failed: {message}")]
    Git { command: String, message: String },

    /// An entry the conformance directory may not hold (§8.1).
    #[error("{}: unexpected entry; {expected}", .path.display())]
    Unexpected { path: PathBuf, expected: String },

    /// A record or the policy does not parse, or breaks a rule of its
    /// format.
    #[error("{} does not load: {message}", .path.display())]
    Parse { path: PathBuf, message: String },

    /// The policy is of another conformance format. A policy is not set
    /// aside like a record, since every condition depends on it.
    #[error(
        "{} is conformance format {found}, but this build reads format {FORMAT}",
        .path.display()
    )]
    FormatMismatch { path: PathBuf, found: u64 },

    /// A record that breaks a rule of its format, refused before it is
    /// written.
    #[error("cannot write {}: {message}", .path.display())]
    Refused { path: PathBuf, message: String },

    /// The spec does not assemble into a model, or the model does not
    /// validate, so it has no proofs and nothing is required of its
    /// code.
    #[error("the spec cannot be verified: {reason}")]
    Unverifiable { reason: String },

    /// The record a write meant to replace is not the one on disk
    /// (§8.4): someone wrote it in the meantime.
    #[error(
        "{} changed since it was read: the write replaces {}, but the file holds {}; read \
         it again",
        .path.display(),
        describe(.expected),
        describe(.found)
    )]
    Changed {
        path: PathBuf,
        expected: Option<Digest>,
        found: Option<Digest>,
    },
}

fn describe(digest: &Option<Digest>) -> String {
    match digest {
        Some(digest) => format!("the record with digest {digest}"),
        None => "no record".to_string(),
    }
}

impl ConformanceError {
    fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();

        move |error| Self::Io { path, error }
    }
}
