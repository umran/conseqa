//! Binding markers (§6 of the conformance revision).
//!
//! A pair of comments binds a region of any text file to a declaration:
//!
//! ```text
//! // conseqa:begin transaction operation.apply_payment/tx.apply_payment
//! …
//! // conseqa:end transaction operation.apply_payment/tx.apply_payment
//! ```
//!
//! No language is parsed. Whatever precedes the keyword is a comment
//! leader, and is ignored. The end marker repeats the key, so a
//! mismatched pair is caught instead of silently closing the wrong
//! region (§6.1). Code, SQL, configuration, and Terraform are bound
//! alike.
//!
//! A region's digest hashes its residual: its own lines, with each
//! region nested in it replaced by one line holding the nested key
//! (§6.3). Digests therefore mirror Part I's partition. Editing a
//! transaction's region drifts the transaction and never the program
//! around it, and re-indenting the transaction's markers drifts
//! neither.
//!
//! A scan reads the files git tracks and the untracked files git does
//! not ignore, or walks the project outside git, and never reads
//! `.conseqa/` (§6.5). It writes nothing. A marker is recognized
//! wherever its keyword appears, and every malformed one is a scan
//! error; a file with one contributes no regions (§6.6). A well-formed
//! key naming no declaration is not a scan error: the code implements
//! something the spec no longer declares, which is a condition of the
//! state (`orphaned`), not of the scan.

use std::fmt;
use std::fs;
use std::path::Path;

use crate::analyzer::basis::{DeclarationKey, TextualKey, TextualKeyError};
use crate::design::change::partition::Partition;
use crate::design::change::side::Repository;
use crate::design::store::{self, PROJECT_DIR, StoreError};

use super::{ConformanceError, Digest, bindable};

/// The keyword that opens a region.
pub const BEGIN: &str = "conseqa:begin";

/// The keyword that closes a region.
pub const END: &str = "conseqa:end";

/// The comment closers that may follow a key.
const CLOSERS: [&str; 2] = ["*/", "-->"];

/// What one scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Every file read, relative to the project directory with `/`
    /// separators, in order.
    pub files: Vec<String>,

    /// The regions of every file without a scan error, by path, then
    /// line.
    pub regions: Vec<Region>,

    /// The scan errors, by path, then line.
    pub errors: Vec<ScanError>,
}

impl Scan {
    /// The regions bound to a declaration.
    pub fn regions_of<'a>(
        &'a self,
        declaration: &'a DeclarationKey,
    ) -> impl Iterator<Item = &'a Region> + 'a {
        self.regions
            .iter()
            .filter(move |region| region.key.key() == declaration)
    }

    /// Whether a file has a scan error. No record whose regions or
    /// evidence lie in it can be written (§6.6).
    pub fn has_error(&self, path: &str) -> bool {
        self.errors.iter().any(|error| error.path == path)
    }

    /// The regions whose keys name no declaration of the spec: code
    /// still implementing something the spec no longer declares. Each is
    /// an `orphaned` condition (§10.1), never a scan error.
    pub fn orphans<'a>(&'a self, partition: &Partition) -> Vec<&'a Region> {
        self.regions
            .iter()
            .filter(|region| !partition.members.contains_key(region.key.key()))
            .collect()
    }
}

/// One region: the lines strictly between a begin marker and its end
/// marker, in one file (§6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The declaration the region binds.
    pub key: TextualKey,

    /// The file, relative to the project directory.
    pub path: String,

    /// The lines of its begin and end markers, counted from 1, so that
    /// an empty region is a range too.
    pub lines: [usize; 2],

    /// The digest of its residual (§6.3).
    pub digest: Digest,
}

/// A scan error (§6.6), at the file and line it was found on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub path: String,
    pub line: usize,
    pub kind: ScanErrorKind,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.kind)
    }
}

/// What is wrong at a scan error. Keys are given as written, since a
/// key that does not parse is one of the errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanErrorKind {
    /// A marker that is not its keyword, whitespace, a key, and then
    /// nothing but whitespace and a comment closer.
    Malformed(Malformed),

    /// The key does not parse.
    Key { key: String, error: TextualKeyError },

    /// The key names a kind no code implements.
    NotBindable { key: String, kind: String },

    /// A begin marker with no end.
    Unclosed { key: String },

    /// An end marker with no begin.
    Unopened { key: String },

    /// An end marker whose key differs from its begin's: it closes the
    /// innermost open region, whose key is `begin`.
    Mismatched {
        begin: String,
        begin_line: usize,
        end: String,
    },

    /// An end marker closing a region while a region begun after it is
    /// still open: the two overlap, and neither contains the other.
    Overlap {
        key: String,
        begin_line: usize,
        other: String,
        other_line: usize,
    },
}

/// How a marker is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Malformed {
    /// Nothing but whitespace and a closer follows the keyword.
    NoKey { keyword: &'static str },

    /// Text runs on from the keyword, with no whitespace between.
    Unseparated {
        keyword: &'static str,
        found: String,
    },

    /// Something other than a closer follows the key.
    Trailing { text: String },
}

impl fmt::Display for ScanErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(Malformed::NoKey { keyword }) => {
                write!(f, "malformed marker: `{keyword}` names no declaration key")
            }

            Self::Malformed(Malformed::Unseparated { keyword, found }) => write!(
                f,
                "malformed marker: `{keyword}{found}`: the keyword is followed by whitespace \
                 and a declaration key"
            ),

            Self::Malformed(Malformed::Trailing { text }) => write!(
                f,
                "malformed marker: `{text}` follows the key, where only a comment closer \
                 (`*/`, `-->`) may"
            ),

            Self::Key { key, error } => write!(f, "`{key}` does not parse: {error}"),

            Self::NotBindable { key, kind } => write!(
                f,
                "`{key}` cannot be bound: no code implements a `{kind}` declaration"
            ),

            Self::Unclosed { key } => write!(f, "`{BEGIN} {key}` has no matching end"),

            Self::Unopened { key } => write!(f, "`{END} {key}` has no matching begin"),

            Self::Mismatched {
                begin,
                begin_line,
                end,
            } => write!(
                f,
                "`{END} {end}` closes the region `{begin}` begun at line {begin_line}; an end \
                 repeats its begin's key"
            ),

            Self::Overlap {
                key,
                begin_line,
                other,
                other_line,
            } => write!(
                f,
                "the region `{key}` begun at line {begin_line} ends inside the region `{other}` \
                 begun at line {other_line}; regions may nest, but never overlap"
            ),
        }
    }
}

/// Scans the project at `project_dir` (§6.5): in a git repository, the
/// files git tracks and the untracked ones it does not ignore; outside
/// one, every file below the project directory but in dot-directories.
/// `.conseqa/` is never read, and neither is a symbolic link, a
/// submodule, a tracked file missing from the working tree, or a file
/// or path that is not UTF-8. Paths are relative to the project
/// directory, which is where the code a project describes lives.
pub fn scan(project_dir: &Path) -> Result<Scan, ConformanceError> {
    let paths = match Repository::of(project_dir) {
        Some(_) => listed(project_dir)?,
        None => walked(project_dir)?,
    };

    let mut scan = Scan::default();

    for path in paths {
        let Some(text) = read_text(&project_dir.join(&path))? else {
            continue;
        };

        let (regions, errors) = scan_file(&path, &text);

        scan.files.push(path);
        scan.regions.extend(regions);
        scan.errors.extend(errors);
    }

    Ok(scan)
}

/// Scans one file's text: its regions, each with its digest, and its
/// scan errors. A file with any error contributes no regions (§6.6).
pub fn scan_file(path: &str, text: &str) -> (Vec<Region>, Vec<ScanError>) {
    // Both keywords begin so: most files are passed over here.
    if !text.contains("conseqa:") {
        return (Vec::new(), Vec::new());
    }

    let lines = lines(text);

    let mut open: Vec<Open> = Vec::new();
    let mut closed: Vec<Closed> = Vec::new();
    let mut errors: Vec<(usize, ScanErrorKind)> = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;

        let Some(marker) = marker(line) else {
            continue;
        };

        match (&marker.malformed, &marker.key) {
            (Some(malformed), _) => {
                errors.push((number, ScanErrorKind::Malformed(malformed.clone())))
            }

            (None, Some(key)) => {
                if let Some(error) = key_error(key) {
                    errors.push((number, error));
                }
            }

            (None, None) => unreachable!("a marker without a key is malformed"),
        }

        // A marker pairs by its key's text whenever it has one, so that
        // a key that fails to parse, or trailing text, is one error,
        // not one more for the marker it would have paired with.
        let Some(key) = marker.key else {
            continue;
        };

        match marker.edge {
            Edge::Begin => open.push(Open {
                key,
                line: number,
                nested: Vec::new(),
            }),

            Edge::End => match open.iter().rposition(|region| region.key == key) {
                Some(index) => {
                    let region = open.remove(index);

                    if let Some(other) = open.get(index) {
                        errors.push((
                            number,
                            ScanErrorKind::Overlap {
                                key: key.clone(),
                                begin_line: region.line,
                                other: other.key.clone(),
                                other_line: other.line,
                            },
                        ));
                    }

                    close(region, number, &mut open, &mut closed);
                }

                None => match open.pop() {
                    Some(region) => {
                        errors.push((
                            number,
                            ScanErrorKind::Mismatched {
                                begin: region.key.clone(),
                                begin_line: region.line,
                                end: key,
                            },
                        ));

                        close(region, number, &mut open, &mut closed);
                    }

                    None => errors.push((number, ScanErrorKind::Unopened { key })),
                },
            },
        }
    }

    for region in open {
        errors.push((region.line, ScanErrorKind::Unclosed { key: region.key }));
    }

    if !errors.is_empty() {
        errors.sort_by_key(|(line, _)| *line);

        let errors = errors
            .into_iter()
            .map(|(line, kind)| ScanError {
                path: path.to_string(),
                line,
                kind,
            })
            .collect();

        return (Vec::new(), errors);
    }

    let mut regions: Vec<Region> = closed
        .iter()
        .map(|region| Region {
            key: region
                .key
                .parse()
                .expect("a file without scan errors has only bindable keys"),
            path: path.to_string(),
            lines: region.lines,
            digest: Digest::of(residual(&lines, region).as_bytes()),
        })
        .collect();

    regions.sort_by_key(|region| region.lines);

    (regions, Vec::new())
}

/// A file's lines without their endings. Only `\n` and `\r\n` end a
/// line, so converting a file's line endings changes no residual
/// (§6.3).
fn lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;

    while let Some(end) = rest.find('\n') {
        let line = &rest[..end];

        lines.push(line.strip_suffix('\r').unwrap_or(line));
        rest = &rest[end + 1..];
    }

    if !rest.is_empty() {
        lines.push(rest);
    }

    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Begin,
    End,
}

/// A marker found on a line, before its key is checked.
struct Marker {
    edge: Edge,

    /// The key as written, what an end pairs with its begin by; `None`
    /// when the marker names none.
    key: Option<String>,

    malformed: Option<Malformed>,
}

/// The marker on a line, if its keyword appears there (§6.1): the
/// keyword, whitespace, the key, and then nothing but whitespace and
/// one comment closer, which may touch the key. A key holds exactly one
/// space, between its kind and its ids.
fn marker(line: &str) -> Option<Marker> {
    let (at, edge, keyword) = match (line.find(BEGIN), line.find(END)) {
        (None, None) => return None,
        (Some(begin), Some(end)) if end < begin => (end, Edge::End, END),
        (Some(begin), _) => (begin, Edge::Begin, BEGIN),
        (None, Some(end)) => (end, Edge::End, END),
    };

    let rest = &line[at + keyword.len()..];

    let mut body = rest.trim_end();

    for closer in CLOSERS {
        if let Some(stripped) = body.strip_suffix(closer) {
            body = stripped.trim_end();
            break;
        }
    }

    let malformed = |malformed| Marker {
        edge,
        key: None,
        malformed: Some(malformed),
    };

    if body.is_empty() {
        return Some(malformed(Malformed::NoKey { keyword }));
    }

    if !body.starts_with(char::is_whitespace) {
        return Some(malformed(Malformed::Unseparated {
            keyword,
            found: rest.trim_end().to_string(),
        }));
    }

    let body = body.trim_start();
    let tokens: Vec<&str> = body.split_whitespace().collect();

    Some(match tokens.as_slice() {
        [_] | [_, _] => Marker {
            edge,
            key: Some(body.to_string()),
            malformed: None,
        },

        [kind, ids, ..] => {
            let after_kind = body[kind.len()..].trim_start();
            let trailing = after_kind[ids.len()..].trim();

            Marker {
                edge,
                key: Some(format!("{kind} {ids}")),
                malformed: Some(Malformed::Trailing {
                    text: trailing.to_string(),
                }),
            }
        }

        [] => unreachable!("the body is not empty"),
    })
}

/// What is wrong with a well-formed marker's key, if anything.
fn key_error(key: &str) -> Option<ScanErrorKind> {
    match DeclarationKey::parse(key) {
        Ok(declaration) if bindable(&declaration) => None,

        Ok(declaration) => Some(ScanErrorKind::NotBindable {
            key: key.to_string(),
            kind: declaration.kind().to_string(),
        }),

        Err(TextualKeyError::NoTextualForm { kind }) => Some(ScanErrorKind::NotBindable {
            key: key.to_string(),
            kind,
        }),

        Err(error) => Some(ScanErrorKind::Key {
            key: key.to_string(),
            error,
        }),
    }
}

/// A region whose end has not been read yet.
struct Open {
    key: String,
    line: usize,
    nested: Vec<Nested>,
}

/// A paired region.
struct Closed {
    key: String,
    lines: [usize; 2],
    nested: Vec<Nested>,
}

/// A region directly nested in another: all its residual keeps of it.
struct Nested {
    key: String,
    lines: [usize; 2],
}

/// Closes `region` at line `end`, nested in the region now innermost,
/// if any.
fn close(region: Open, end: usize, open: &mut [Open], closed: &mut Vec<Closed>) {
    let lines = [region.line, end];

    if let Some(parent) = open.last_mut() {
        parent.nested.push(Nested {
            key: region.key.clone(),
            lines,
        });
    }

    closed.push(Closed {
        key: region.key,
        lines,
        nested: region.nested,
    });
}

/// A region's residual (§6.3): its lines, each ended by `\n`, with each
/// directly nested region, markers included, replaced by one line
/// holding exactly its key.
fn residual(lines: &[&str], region: &Closed) -> String {
    let mut text = String::new();
    let mut nested = region.nested.iter().peekable();
    let mut number = region.lines[0] + 1;

    while number < region.lines[1] {
        match nested.peek() {
            Some(inner) if inner.lines[0] == number => {
                text.push_str(&inner.key);
                number = inner.lines[1] + 1;
                nested.next();
            }

            _ => {
                text.push_str(lines[number - 1]);
                number += 1;
            }
        }

        text.push('\n');
    }

    text
}

/// The files git lists below the project directory: tracked, and
/// untracked but not ignored.
fn listed(project_dir: &Path) -> Result<Vec<String>, ConformanceError> {
    let output = store::git(
        project_dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        None,
    )
    .map_err(|error| match error {
        StoreError::Git { command, message } => ConformanceError::Git { command, message },
        other => ConformanceError::Git {
            command: "ls-files".to_string(),
            message: other.to_string(),
        },
    })?;

    let mut paths: Vec<String> = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        // A path that is not UTF-8 cannot be named in a record.
        .filter_map(|record| String::from_utf8(record.to_vec()).ok())
        .filter(|path| !in_project_dir(path))
        .collect();

    // A conflicted path is listed once per stage.
    paths.sort();
    paths.dedup();

    Ok(paths)
}

fn in_project_dir(path: &str) -> bool {
    path.strip_prefix(PROJECT_DIR)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Every file below the project directory, outside git, but in
/// dot-directories: `.conseqa/` among them.
fn walked(project_dir: &Path) -> Result<Vec<String>, ConformanceError> {
    let mut paths = Vec::new();

    walk(project_dir, "", &mut paths)?;

    paths.sort();

    Ok(paths)
}

fn walk(dir: &Path, prefix: &str, paths: &mut Vec<String>) -> Result<(), ConformanceError> {
    for entry in fs::read_dir(dir).map_err(ConformanceError::io(dir))? {
        let entry = entry.map_err(ConformanceError::io(dir))?;

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };

        // Not followed: a symbolic link is neither.
        let kind = entry
            .file_type()
            .map_err(ConformanceError::io(entry.path()))?;

        if kind.is_dir() {
            if !name.starts_with('.') {
                walk(&entry.path(), &format!("{prefix}{name}/"), paths)?;
            }
        } else if kind.is_file() {
            paths.push(format!("{prefix}{name}"));
        }
    }

    Ok(())
}

/// A listed file's text, or `None` when there is none to scan.
fn read_text(path: &Path) -> Result<Option<String>, ConformanceError> {
    let missing = |error: &std::io::Error| error.kind() == std::io::ErrorKind::NotFound;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(ConformanceError::io(path)(error)),
    }

    match fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8(bytes).ok()),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(ConformanceError::io(path)(error)),
    }
}
