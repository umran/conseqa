//! Binding markers, conformance records, and the policy (§6, §8, and
//! §14.4 of the conformance and implementation guidance revision; its
//! tests §18.1–§18.3).

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Value, json};

use conseqa::{
    analyzer::basis::{
        DeclarationKey, Fact, FactFormat, TextualKey, TextualKeyError, read_canonical,
    },
    conformance::{
        ConformanceError, Digest,
        markers::{self, Malformed, Region, Scan, ScanError, ScanErrorKind},
        policy::{Policy, ReviewRule},
        record::{
            Attestation, AttestedFact, ClaimScope, CompletenessClaim, Exception, Party, PartyKind,
            Record, RecordContent, RecordedRegion, Records, Review, ReviewItem, ReviewTarget, Span,
            Verdict, VersionMismatch,
        },
    },
    design::{ProjectStore, RunId, RunMetadata, WorkspaceState, change::partition::Partition},
    parser::yaml,
    spec::DslVersion,
};

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "conseqa-conformance-{label}-{}",
        uuid::Uuid::new_v4()
    ));

    fs::create_dir_all(&dir).unwrap();

    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Conseqa Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=master",
        ])
        .args(args)
        .output()
        .expect("git runs");

    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).unwrap()
}

/// Writes `contents` at `path` below `dir`, making its directories.
fn put(dir: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(path);

    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn fixture(name: &str) -> WorkspaceState {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);

    let model = yaml::parse(&fs::read_to_string(path).unwrap()).unwrap();

    WorkspaceState::from_model(&model, RunMetadata::new(RunId(name.to_string())))
}

fn key(text: &str) -> TextualKey {
    text.parse().unwrap()
}

/// A region of `key` around `body`, with `//` markers.
fn bound(key: &str, body: &str) -> String {
    format!("// conseqa:begin {key}\n{body}// conseqa:end {key}\n")
}

fn scan_text(text: &str) -> (Vec<Region>, Vec<ScanError>) {
    markers::scan_file("src/lib.rs", text)
}

/// The scan errors of a text, by line.
fn errors(text: &str) -> Vec<(usize, ScanErrorKind)> {
    let (regions, errors) = scan_text(text);

    assert!(
        regions.is_empty(),
        "a file with a scan error contributes no regions"
    );

    errors
        .into_iter()
        .map(|error| (error.line, error.kind))
        .collect()
}

/// Each region's digest, by its key.
fn digests(text: &str) -> BTreeMap<String, Digest> {
    let (regions, errors) = scan_text(text);

    assert!(errors.is_empty(), "{errors:?}");

    regions
        .into_iter()
        .map(|region| (region.key.to_string(), region.digest))
        .collect()
}

/// The keys of a scan's regions, in order.
fn region_keys(scan: &Scan) -> Vec<String> {
    scan.regions
        .iter()
        .map(|region| region.key.to_string())
        .collect()
}

// ---------------------------------------------------------------------
// §18.1 Markers
// ---------------------------------------------------------------------

/// Whatever precedes the keyword is a comment leader (§6.1).
#[test]
fn every_comment_leader_binds_a_region() {
    let leaders = [
        ("//", ""),
        ("#", ""),
        ("--", ""),
        (";", ""),
        ("/*", " */"),
        ("<!--", " -->"),
        ("    let total = 0; //", ""),
    ];

    for (leader, closer) in leaders {
        let text = format!(
            "{leader} conseqa:begin topic topic.orders{closer}\nbody\n{leader} conseqa:end \
             topic topic.orders{closer}\n"
        );

        let (regions, errors) = scan_text(&text);

        assert!(errors.is_empty(), "`{leader}`: {errors:?}");
        assert_eq!(regions.len(), 1, "`{leader}`");
        assert_eq!(regions[0].key, key("topic topic.orders"));
        assert_eq!(regions[0].path, "src/lib.rs");
        assert_eq!(regions[0].lines, [1, 3]);
        assert_eq!(regions[0].digest, Digest::of(b"body\n"));
    }
}

/// A closer may follow the key after whitespace or touch it.
#[test]
fn a_comment_closer_may_follow_the_key_or_touch_it() {
    let text = "/* conseqa:begin topic topic.orders*/  \nbody\n<!-- conseqa:end topic \
                topic.orders-->\n";

    let (regions, errors) = scan_text(text);

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].key, key("topic topic.orders"));
}

/// A marker is recognized wherever its keyword appears, so anything but
/// the keyword, whitespace, a key, and a closer is a named error (§6.6).
#[test]
fn malformed_markers_are_scan_errors() {
    let cases = [
        (
            "// conseqa:begin\n".to_string(),
            Malformed::NoKey {
                keyword: "conseqa:begin",
            },
        ),
        (
            "/* conseqa:end */\n".to_string(),
            Malformed::NoKey {
                keyword: "conseqa:end",
            },
        ),
        (
            "// conseqa:beginning of the region\n".to_string(),
            Malformed::Unseparated {
                keyword: "conseqa:begin",
                found: "ning of the region".to_string(),
            },
        ),
        (
            "let marker = \"conseqa:begin\";\n".to_string(),
            Malformed::Unseparated {
                keyword: "conseqa:begin",
                found: "\";".to_string(),
            },
        ),
        // A marker with trailing text still pairs by its key, so its
        // end raises nothing more.
        (
            "// conseqa:begin topic topic.orders and more\nbody\n// conseqa:end topic \
             topic.orders\n"
                .to_string(),
            Malformed::Trailing {
                text: "and more".to_string(),
            },
        ),
        (
            "/* conseqa:begin topic topic.orders */ start();\nbody\n// conseqa:end topic \
             topic.orders\n"
                .to_string(),
            Malformed::Trailing {
                text: "*/ start();".to_string(),
            },
        ),
    ];

    for (text, malformed) in cases {
        assert_eq!(
            errors(&text),
            vec![(1, ScanErrorKind::Malformed(malformed))],
            "{text}"
        );
    }
}

/// A key that does not parse, or names a kind no code implements, is an
/// error at each marker carrying it, and no pairing error follows.
#[test]
fn a_bad_key_is_an_error_at_each_of_its_markers_and_nothing_more() {
    let unknown = "widget widget.a";
    let short = "transaction operation.a";
    let tabbed = "topic\ttopic.a";
    let escaped = "topic topic%2Ea";

    let cases = [
        (
            unknown,
            ScanErrorKind::Key {
                key: unknown.to_string(),
                error: TextualKeyError::UnknownKind {
                    kind: "widget".to_string(),
                },
            },
        ),
        (
            short,
            ScanErrorKind::Key {
                key: short.to_string(),
                error: TextualKeyError::WrongArity {
                    kind: "transaction".to_string(),
                    expected: 2,
                    found: 1,
                },
            },
        ),
        (
            tabbed,
            ScanErrorKind::Key {
                key: tabbed.to_string(),
                error: TextualKeyError::Malformed {
                    text: tabbed.to_string(),
                },
            },
        ),
        (
            escaped,
            ScanErrorKind::Key {
                key: escaped.to_string(),
                error: TextualKeyError::NotCanonical {
                    segment: "topic%2Ea".to_string(),
                },
            },
        ),
        (
            "requirement operation.a",
            ScanErrorKind::NotBindable {
                key: "requirement operation.a".to_string(),
                kind: "requirement".to_string(),
            },
        ),
        (
            "prompt_obligation obligation.a",
            ScanErrorKind::NotBindable {
                key: "prompt_obligation obligation.a".to_string(),
                kind: "prompt_obligation".to_string(),
            },
        ),
    ];

    for (key, error) in cases {
        assert_eq!(
            errors(&bound(key, "body\n")),
            vec![(1, error.clone()), (3, error)],
            "{key}"
        );
    }
}

/// An end closes the innermost open region; one naming no open region
/// is mismatched, one naming an outer region crosses an inner one.
#[test]
fn unpaired_and_crossed_markers_are_scan_errors() {
    let a = "// conseqa:begin topic topic.a\n";
    let b = "// conseqa:begin topic topic.b\n";
    let end_a = "// conseqa:end topic topic.a\n";
    let end_b = "// conseqa:end topic topic.b\n";

    assert_eq!(
        errors(&format!("{a}{end_b}")),
        vec![(
            2,
            ScanErrorKind::Mismatched {
                begin: "topic topic.a".to_string(),
                begin_line: 1,
                end: "topic topic.b".to_string(),
            }
        )]
    );

    assert_eq!(
        errors(&format!("{a}body\n")),
        vec![(
            1,
            ScanErrorKind::Unclosed {
                key: "topic topic.a".to_string()
            }
        )]
    );

    assert_eq!(
        errors(&format!("body\n{end_a}")),
        vec![(
            2,
            ScanErrorKind::Unopened {
                key: "topic topic.a".to_string()
            }
        )]
    );

    assert_eq!(
        errors(&format!("{a}{b}{end_a}{end_b}")),
        vec![(
            3,
            ScanErrorKind::Overlap {
                key: "topic topic.a".to_string(),
                begin_line: 1,
                other: "topic topic.b".to_string(),
                other_line: 2,
            }
        )]
    );
}

/// One scan error withholds every region of its file (§6.6).
#[test]
fn a_file_with_a_scan_error_contributes_no_regions() {
    let text = format!(
        "{}// conseqa:begin topic topic.b\n",
        bound("topic topic.a", "body\n")
    );

    assert_eq!(
        errors(&text),
        vec![(
            4,
            ScanErrorKind::Unclosed {
                key: "topic topic.b".to_string()
            }
        )]
    );
}

const NESTED: &str = "\
// conseqa:begin program operation.ship
fn ship() {
    // conseqa:begin transaction operation.ship/tx.reserve
    reserve();
    // conseqa:end transaction operation.ship/tx.reserve
    notify();
    // conseqa:begin transaction operation.ship/tx.record
    record();
    // conseqa:end transaction operation.ship/tx.record
}
// conseqa:end program operation.ship
";

/// Regions nest, and the outer residual holds one line per inner
/// region: exactly its key (§6.3).
#[test]
fn regions_nest_and_an_outer_residual_holds_each_inner_key() {
    let (regions, errors) = scan_text(NESTED);

    assert!(errors.is_empty(), "{errors:?}");

    let found: Vec<(String, [usize; 2])> = regions
        .iter()
        .map(|region| (region.key.to_string(), region.lines))
        .collect();

    assert_eq!(
        found,
        vec![
            ("program operation.ship".to_string(), [1, 11]),
            ("transaction operation.ship/tx.reserve".to_string(), [3, 5]),
            ("transaction operation.ship/tx.record".to_string(), [7, 9]),
        ]
    );

    assert_eq!(
        regions[0].digest,
        Digest::of(
            b"fn ship() {\ntransaction operation.ship/tx.reserve\n    notify();\n\
              transaction operation.ship/tx.record\n}\n"
        )
    );

    assert_eq!(regions[1].digest, Digest::of(b"    reserve();\n"));
}

/// A well-formed key naming no declaration is an orphan of the current
/// spec, never a scan error (§6.6, §10.1).
#[test]
fn a_key_naming_no_declaration_is_an_orphan_not_a_scan_error() {
    let text = format!(
        "{}{}",
        bound(
            "transaction operation.apply_payment/tx.apply_payment",
            "apply();\n"
        ),
        bound("transaction operation.refund/tx.refund", "refund();\n")
    );

    let (regions, errors) = scan_text(&text);

    assert!(errors.is_empty(), "{errors:?}");

    let scan = Scan {
        files: vec!["src/lib.rs".to_string()],
        regions,
        errors,
    };

    let partition = Partition::of(&fixture("flash_checkout.yaml"));

    let orphans: Vec<String> = scan
        .orphans(&partition)
        .into_iter()
        .map(|region| region.key.to_string())
        .collect();

    assert_eq!(orphans, vec!["transaction operation.refund/tx.refund"]);
}

/// The scan reads what git tracks and what it would track, never what
/// it ignores, never `.conseqa/`, and never a symbolic link, a missing
/// file, or a file that is not UTF-8 (§6.5).
#[test]
fn the_scan_reads_tracked_and_unignored_files_and_never_conseqa() {
    let root = temp_dir("scan");

    git(&root, &["init", "-q"]);

    put(&root, ".gitignore", "ignored.rs\nvendored.rs\n");
    put(&root, "src/tracked.rs", bound("topic topic.tracked", "a\n"));
    put(&root, "vendored.rs", bound("topic topic.vendored", "b\n"));
    put(&root, "src/gone.rs", bound("topic topic.gone", "c\n"));

    git(&root, &["add", ".gitignore", "src"]);
    git(&root, &["add", "-f", "vendored.rs"]);
    git(&root, &["commit", "-q", "-m", "files"]);

    fs::remove_file(root.join("src/gone.rs")).unwrap();

    put(
        &root,
        "untracked.sql",
        bound("topic topic.untracked", "d\n"),
    );
    put(&root, "ignored.rs", bound("topic topic.ignored", "e\n"));
    put(&root, ".conseqa/notes.md", "the `conseqa:begin` marker\n");

    let mut binary = vec![0xff, 0xfe, b'\n'];
    binary.extend(bound("topic topic.binary", "f\n").into_bytes());
    put(&root, "binary.bin", binary);

    #[cfg(unix)]
    std::os::unix::fs::symlink("src/tracked.rs", root.join("link.rs")).unwrap();

    let scan = markers::scan(&root).unwrap();

    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(
        scan.files,
        vec![
            ".gitignore",
            "src/tracked.rs",
            "untracked.sql",
            "vendored.rs"
        ]
    );
    assert_eq!(
        region_keys(&scan),
        vec![
            "topic topic.tracked",
            "topic topic.untracked",
            "topic topic.vendored"
        ]
    );
}

/// Outside git, the scan walks the project, passing over dot-directories
/// but not dotfiles.
#[test]
fn outside_git_the_scan_walks_the_project_but_dot_directories() {
    let root = temp_dir("walk");

    put(&root, "src/a.rs", bound("topic topic.a", "a\n"));
    put(&root, "target/b.rs", bound("topic topic.b", "b\n"));
    put(&root, ".env", bound("topic topic.env", "c\n"));
    put(&root, ".hidden/d.rs", bound("topic topic.hidden", "d\n"));
    put(&root, ".conseqa/notes.md", "the `conseqa:begin` marker\n");

    let scan = markers::scan(&root).unwrap();

    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(scan.files, vec![".env", "src/a.rs", "target/b.rs"]);
    assert_eq!(
        region_keys(&scan),
        vec!["topic topic.env", "topic topic.a", "topic topic.b"]
    );
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];

    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();

            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }

    files
}

/// A scan reads only: the repository, its index included, is
/// byte-identical after one (§6.5).
#[test]
fn a_scan_writes_nothing() {
    let root = temp_dir("read-only");

    git(&root, &["init", "-q"]);
    put(&root, "src/a.rs", NESTED);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "a"]);
    put(
        &root,
        "src/a.rs",
        NESTED.replace("notify()", "notify(order)"),
    );

    let before = snapshot(&root);

    markers::scan(&root).unwrap();

    assert_eq!(snapshot(&root), before);
}

// ---------------------------------------------------------------------
// §18.2 Digests
// ---------------------------------------------------------------------

/// The keys whose digests differ between two texts.
fn drifted(base: &str, head: &str) -> Vec<String> {
    let base = digests(base);
    let head = digests(head);

    assert_eq!(
        base.keys().collect::<Vec<_>>(),
        head.keys().collect::<Vec<_>>()
    );

    base.into_iter()
        .filter(|(key, digest)| head[key] != *digest)
        .map(|(key, _)| key)
        .collect()
}

#[test]
fn editing_a_nested_region_drifts_only_the_inner_declaration() {
    assert_eq!(
        drifted(NESTED, &NESTED.replace("reserve();", "reserve(order);")),
        vec!["transaction operation.ship/tx.reserve"]
    );
}

#[test]
fn editing_the_outer_regions_own_lines_drifts_only_the_outer_declaration() {
    assert_eq!(
        drifted(NESTED, &NESTED.replace("notify();", "notify(order);")),
        vec!["program operation.ship"]
    );
}

#[test]
fn editing_outside_every_region_drifts_nothing() {
    let moved = format!("use std::io;\n\n{NESTED}\nfn main() {{}}\n");

    assert!(drifted(NESTED, &moved).is_empty());
}

/// Line endings are normalized, and an inner region's markers are not
/// part of the outer residual: only its key is.
#[test]
fn line_endings_and_inner_marker_layout_drift_nothing() {
    assert!(drifted(NESTED, &NESTED.replace('\n', "\r\n")).is_empty());

    let recommented = NESTED
        .replace(
            "    // conseqa:begin transaction operation.ship/tx.reserve",
            "/* conseqa:begin transaction operation.ship/tx.reserve */",
        )
        .replace(
            "    // conseqa:end transaction operation.ship/tx.reserve",
            "        # conseqa:end transaction operation.ship/tx.reserve",
        );

    assert!(drifted(NESTED, &recommented).is_empty());
}

// ---------------------------------------------------------------------
// §18.3 Records
// ---------------------------------------------------------------------

const APPLY: &str = "transaction operation.apply_payment/tx.apply_payment";
const ORDER: &str = "data_object data.checkout/object.order";

/// Every sample fact of the golden files, read canonically.
fn golden_facts() -> Vec<Fact> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/facts");

    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();

    paths.sort();

    let mut facts = Vec::new();

    for path in paths {
        let golden: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        for fact in golden["facts"].as_array().unwrap() {
            facts.push(read_canonical(fact).unwrap());
        }
    }

    facts
}

fn party(kind: PartyKind, name: &str) -> Party {
    Party {
        kind,
        name: name.to_string(),
    }
}

fn region(path: &str, lines: [usize; 2], text: &str) -> RecordedRegion {
    RecordedRegion {
        path: path.to_string(),
        lines,
        digest: Digest::of(text.as_bytes()),
    }
}

/// A record of `declaration`, holding its residual in the flash checkout
/// fixture, with two regions, one attested fact, and a review.
fn record(declaration: &str) -> Record {
    let declaration = key(declaration);

    let residual =
        Partition::of(&fixture("flash_checkout.yaml")).members[declaration.key()].clone();

    let mut record = Record::new(
        declaration,
        residual,
        Attestation {
            by: party(PartyKind::Agent, "claude-opus-5-5"),
            commit: Some("8883890".to_string()),
        },
    );

    record.regions = vec![
        region("src/payments/apply.rs", [41, 72], "apply"),
        region("migrations/0007_orders_version.sql", [1, 12], "alter"),
    ];

    record.structural = true;

    record.facts = vec![AttestedFact {
        fact: Fact::NoDeleters {
            data_model: conseqa::spec::Id("data.checkout".to_string()),
            object: conseqa::spec::Id("object.order".to_string()),
        },
        evidence: vec![Span {
            path: "src/payments/apply.rs".to_string(),
            lines: [47, 51],
        }],
    }];

    record.reviews = vec![Review {
        by: party(PartyKind::Agent, "codex"),
        digests: record.digests(),
        verdict: Verdict::Confirmed,
        items: vec![
            ReviewItem {
                target: ReviewTarget::Fact(0),
                verdict: Verdict::Confirmed,
                note: Some("WHERE version = $2 at 50".to_string()),
            },
            ReviewItem {
                target: ReviewTarget::Structural,
                verdict: Verdict::Confirmed,
                note: None,
            },
        ],
    }];

    record
}

fn read(records: &Records, declaration: &str) -> RecordContent {
    records.read_all().unwrap()[&key(declaration)]
        .content
        .clone()
}

/// Every fact kind survives a record: written in its durable form, read
/// back strictly (§8.2).
#[test]
fn a_record_round_trips_with_every_fact_kind() {
    let project = temp_dir("round-trip");
    let records = Records::of_project(&project);

    let mut record = record(APPLY);

    record.facts = golden_facts()
        .into_iter()
        .map(|fact| AttestedFact {
            fact,
            evidence: vec![Span {
                path: "src/payments/apply.rs".to_string(),
                lines: [47, 51],
            }],
        })
        .collect();

    let digest = records.write(&record, None).unwrap();

    let path = project
        .join(".conseqa/conformance/transaction/operation.apply_payment/tx.apply_payment.yaml");

    assert_eq!(records.path(&key(APPLY)), path);

    let text = fs::read_to_string(&path).unwrap();

    assert_eq!(digest, Digest::of(text.as_bytes()));
    assert!(
        text.starts_with(&format!(
            "format: 1\ndsl: 6\nfact_format: 1\ndeclaration: {APPLY}\nresidual:\n"
        )),
        "{text}"
    );

    let all = records.read_all().unwrap();

    assert_eq!(all.len(), 1);
    assert_eq!(all[&key(APPLY)].digest, digest);
    assert_eq!(
        all[&key(APPLY)].content,
        RecordContent::Read(Box::new(record.clone()))
    );
    assert_eq!(
        records.read(&key(APPLY)).unwrap().as_ref(),
        all.get(&key(APPLY))
    );

    // The same record is the same bytes, in any project.
    let elsewhere = Records::of_project(&temp_dir("round-trip-again"));

    assert_eq!(elsewhere.write(&record, None).unwrap(), digest);
}

/// A data object's record carries writers completeness, and a review may
/// address a claim.
#[test]
fn a_data_object_record_carries_its_completeness_claims() {
    let records = Records::of_project(&temp_dir("completeness"));

    let mut record = record(ORDER);

    record.completeness = vec![CompletenessClaim {
        scope: ClaimScope::Writers,
        patterns: vec![
            r"(?i)\b(update|delete\s+from|insert\s+into)\s+orders\b".to_string(),
            r"\bOrderRepo::(save|delete)\b".to_string(),
        ],
        exceptions: vec![Exception {
            path: "tools/backfill_versions.rs".to_string(),
            reason: "one-off backfill, not deployed".to_string(),
        }],
    }];

    record.reviews[0].items.push(ReviewItem {
        target: ReviewTarget::Completeness(0),
        verdict: Verdict::Disputed,
        note: Some("misses the raw SQL in reports/".to_string()),
    });

    records.write(&record, None).unwrap();

    assert_eq!(read(&records, ORDER), RecordContent::Read(Box::new(record)));
}

/// Ids are escaped in a record's path as in its key, so any id is a
/// path segment.
#[test]
fn a_records_path_is_its_textual_key() {
    let records = Records::of_project(Path::new("/project"));

    assert_eq!(
        records.path(&key("topic topic.order_events")),
        Path::new("/project/.conseqa/conformance/topic/topic.order_events.yaml")
    );

    assert_eq!(
        records.path(&key("effect_site operation.ship/effect%2Fnotify")),
        Path::new("/project/.conseqa/conformance/effect_site/operation.ship/effect%2Fnotify.yaml")
    );
}

/// Each entry the conformance directory may not hold is refused by name
/// (§8.1).
#[test]
fn the_reader_refuses_what_the_directory_may_not_hold() {
    let cases = [
        ("notes.md", "notes.md"),
        ("widgets/widget.a.yaml", "widgets"),
        ("requirement/operation.a.yaml", "requirement"),
        ("prompt_obligation/obligation.a.yaml", "prompt_obligation"),
        ("topic/topic%2Eorders.yaml", "topic/topic%2Eorders.yaml"),
        ("topic/topic.orders.yml", "topic/topic.orders.yml"),
        ("transaction/tx.a.yaml", "transaction/tx.a.yaml"),
        ("topic/topic.orders/x.yaml", "topic/topic.orders"),
        (
            "transaction/operation.a/tx.b/x.yaml",
            "transaction/operation.a/tx.b",
        ),
        ("policy.yaml/x.yaml", "policy.yaml"),
    ];

    for (entry, refused) in cases {
        let project = temp_dir("strict");

        put(
            &project,
            &format!(".conseqa/conformance/{entry}"),
            "format: 1\n",
        );

        match Records::of_project(&project).read_all() {
            Err(ConformanceError::Unexpected { path, .. }) => assert_eq!(
                path,
                project.join(".conseqa/conformance").join(refused),
                "{entry}"
            ),

            other => panic!("{entry}: {other:?}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_among_the_records_is_refused() {
    let project = temp_dir("strict-link");
    let records = Records::of_project(&project);

    records.write(&record(APPLY), None).unwrap();

    let link = project.join(".conseqa/conformance/topic/topic.order_events.yaml");

    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(records.path(&key(APPLY)), &link).unwrap();

    assert!(matches!(
        records.read_all(),
        Err(ConformanceError::Unexpected { path, .. }) if path == link
    ));
}

/// Dotfiles are an editor's or the OS's, and pass at every level; so do
/// a write's temporaries and the policy.
#[test]
fn dotfiles_and_the_policy_pass() {
    let project = temp_dir("dotfiles");
    let records = Records::of_project(&project);

    records.write(&record(APPLY), None).unwrap();

    for dir in ["", "transaction/", "transaction/operation.apply_payment/"] {
        put(
            &project,
            &format!(".conseqa/conformance/{dir}.DS_Store"),
            "",
        );
    }

    put(
        &project,
        ".conseqa/conformance/transaction/operation.apply_payment/.tx.apply_payment.yaml.tmp-7",
        "torn",
    );
    put(&project, ".conseqa/conformance/policy.yaml", "format: 1\n");

    assert_eq!(
        records.read_all().unwrap().into_keys().collect::<Vec<_>>(),
        vec![key(APPLY)]
    );
}

/// A record's YAML as a value: what a hand edit changes.
fn as_value(record: &Record) -> Value {
    serde_json::to_value(record).unwrap()
}

/// Writes `value` as the record of `declaration`, by hand.
fn hand_write(project: &Path, declaration: &str, value: &Value) {
    let path = Records::of_project(project).path(&key(declaration));

    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_yaml::to_string(value).unwrap()).unwrap();
}

/// A record that does not parse, or breaks a rule of its format, fails
/// the read, naming the file and what is wrong.
#[test]
fn a_record_that_breaks_its_format_is_refused_by_name() {
    let valid = as_value(&record(APPLY));

    let edit = |change: &dyn Fn(&mut Value)| {
        let mut value = valid.clone();

        change(&mut value);

        value
    };

    let cases: Vec<(Value, &str)> = vec![
        (
            edit(&|value| value["owner"] = json!("me")),
            "unknown field `owner`",
        ),
        (
            edit(&|value| {
                value.as_object_mut().unwrap().remove("format");
            }),
            "missing `format`",
        ),
        (
            edit(&|value| {
                value.as_object_mut().unwrap().remove("dsl");
            }),
            "declares no dsl",
        ),
        (
            edit(&|value| {
                value.as_object_mut().unwrap().remove("fact_format");
            }),
            "declares no fact_format",
        ),
        (
            edit(&|value| value["declaration"] = json!("transaction operation.a/tx.a")),
            "but its path names",
        ),
        (
            edit(&|value| value["facts"][0]["fact"]["owner"] = json!("me")),
            "unknown field",
        ),
        (
            edit(&|value| value["facts"][0]["evidence"] = json!([])),
            "cites no evidence",
        ),
        (
            edit(&|value| {
                let first = value["facts"][0].clone();

                value["facts"].as_array_mut().unwrap().push(first);
            }),
            "repeats an earlier fact",
        ),
        (
            edit(&|value| {
                value["completeness"] = json!([{ "scope": "writers", "patterns": ["orders"] }])
            }),
            "belongs on a `data_object` record",
        ),
        (
            edit(&|value| value["reviews"][0]["items"][0]["fact"] = json!(5)),
            "names facts[5], but the record has 1",
        ),
        (
            edit(&|value| value["reviews"][0]["items"][0]["structural"] = json!(true)),
            "exactly one of",
        ),
        (
            edit(&|value| value["regions"][0]["lines"] = json!([5, 5])),
            "begin and end marker lines",
        ),
        (
            edit(&|value| value["regions"][0]["path"] = json!("../outside.rs")),
            "not a path relative to the project directory",
        ),
        (
            edit(&|value| {
                value["regions"][0]["digest"] = json!(Digest::of(b"x").to_hex().to_uppercase())
            }),
            "is not a digest",
        ),
        (
            edit(&|value| value["attested"]["by"]["kind"] = json!("robot")),
            "unknown variant `robot`",
        ),
    ];

    for (value, expected) in cases {
        let project = temp_dir("refused");

        hand_write(&project, APPLY, &value);

        match Records::of_project(&project).read_all() {
            Err(error @ ConformanceError::Parse { .. }) => {
                let message = error.to_string();

                assert!(
                    message.contains(expected) && message.contains("tx.apply_payment.yaml"),
                    "expected `{expected}`: {message}"
                );
            }

            other => panic!("expected `{expected}`: {other:?}"),
        }
    }
}

/// A record written on another version axis is set aside unread, even
/// with fields this build's format lacks: its declaration reads as
/// `unattested`, for a reason naming both versions (§8.6).
#[test]
fn a_record_of_another_version_is_set_aside_with_its_reason() {
    let cases = [
        (
            "format",
            VersionMismatch::Format { found: 2 },
            "format 2",
            "format 1",
        ),
        (
            "dsl",
            VersionMismatch::Dsl {
                found: DslVersion(2),
            },
            "dsl 2",
            "dsl 6",
        ),
        (
            "fact_format",
            VersionMismatch::FactFormat {
                found: FactFormat(2),
            },
            "fact format 2",
            "fact format 1",
        ),
    ];

    for (field, mismatch, found, reads) in cases {
        let project = temp_dir("version");

        let mut value = as_value(&record(APPLY));

        value[field] = json!(2);
        value["realization"] = json!({ "entry": "kafka.keyed-topic@1", "options": [] });

        hand_write(&project, APPLY, &value);

        let file = &Records::of_project(&project).read_all().unwrap()[&key(APPLY)];

        assert_eq!(file.content, RecordContent::OtherVersion(mismatch));

        let reason = mismatch.to_string();

        assert!(reason.contains(found) && reason.contains(reads), "{reason}");
    }
}

/// A write names the record it replaces, and is refused when the file
/// holds another (§8.4).
#[test]
fn a_write_is_refused_when_the_record_changed_since_it_was_read() {
    let records = Records::of_project(&temp_dir("optimistic"));
    let record = record(APPLY);
    let path = records.path(&key(APPLY));

    let first = records.write(&record, None).unwrap();

    // A second writer that read no record finds one.
    assert!(matches!(
        records.write(&record, None),
        Err(ConformanceError::Changed { expected: None, found: Some(found), .. }) if found == first
    ));

    // A hand edit moves the file under a writer that read it before.
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("structural: true", "structural: false");

    fs::write(&path, &edited).unwrap();

    let edited = Digest::of(edited.as_bytes());

    assert!(matches!(
        records.write(&record, Some(first)),
        Err(ConformanceError::Changed { expected: Some(expected), found: Some(found), .. })
            if expected == first && found == edited
    ));

    assert_eq!(records.write(&record, Some(edited)).unwrap(), first);

    // A deleted record leaves nothing to replace.
    fs::remove_file(&path).unwrap();

    assert!(matches!(
        records.write(&record, Some(first)),
        Err(ConformanceError::Changed {
            expected: Some(_),
            found: None,
            ..
        })
    ));
}

/// Two writers that read the same record never both replace it: the
/// comparison and the rename are one step.
#[test]
fn concurrent_writers_never_both_replace_one_record() {
    let records = Records::of_project(&temp_dir("concurrent"));
    let record = record(APPLY);

    let results: Vec<Result<Digest, ConformanceError>> = std::thread::scope(|scope| {
        let writers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| records.write(&record, None)))
            .collect();

        writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect()
    });

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.iter().all(|result| matches!(
        result,
        Ok(_)
            | Err(ConformanceError::Changed {
                expected: None,
                found: Some(_),
                ..
            })
    )));
}

/// Hand edits are authoritative on read, wrong digests included (§8.4).
#[test]
fn a_hand_edited_record_is_adopted() {
    let project = temp_dir("hand-edit");
    let records = Records::of_project(&project);
    let mut record = record(APPLY);

    // No review repeats the region digest the edit replaces.
    record.reviews.clear();

    records.write(&record, None).unwrap();

    let path = records.path(&key(APPLY));
    let stale = Digest::of(b"code nobody read");

    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("structural: true", "structural: false")
        .replace(&Digest::of(b"apply").to_hex(), &stale.to_hex());

    fs::write(&path, &edited).unwrap();

    record.structural = false;
    record.regions[0].digest = stale;

    let file = &records.read_all().unwrap()[&key(APPLY)];

    assert_eq!(file.digest, Digest::of(edited.as_bytes()));
    assert_eq!(file.content, RecordContent::Read(Box::new(record)));
}

/// A review applies while its digests, as a multiset, are the record's,
/// and a rebind drops every review that saw other code (§14.2, §14.3).
#[test]
fn reviews_apply_to_the_code_they_saw_and_a_rebind_drops_the_rest() {
    let mut record = record(APPLY);

    let [apply, alter] = [record.regions[0].digest, record.regions[1].digest];
    let edited = Digest::of(b"apply, edited");

    let review = |digests: Vec<Digest>| Review {
        by: party(PartyKind::Person, "reviewer"),
        digests,
        verdict: Verdict::Confirmed,
        items: Vec::new(),
    };

    record.reviews = vec![
        review(vec![alter, apply]),
        review(vec![apply]),
        review(vec![apply, alter, alter]),
        review(vec![edited, alter]),
    ];

    let applies: Vec<bool> = record
        .reviews
        .iter()
        .map(|review| record.reviewed_as_recorded(review))
        .collect();

    assert_eq!(applies, vec![true, false, false, false]);

    let mut regions = record.regions.clone();

    regions[0].digest = edited;
    record.rebind(regions);

    assert_eq!(record.reviews, vec![review(vec![edited, alter])]);
}

/// A write refuses anything its reader would refuse, and writes nothing.
#[test]
fn a_write_refuses_what_its_reader_would() {
    let records = Records::of_project(&temp_dir("write-refused"));

    let mut old = record(APPLY);
    old.dsl = DslVersion(5);

    let mut unevidenced = record(APPLY);
    unevidenced.facts[0].evidence.clear();

    let mut intent = record(APPLY);
    intent.declaration = key("prompt_obligation obligation.paid_once");

    for (record, expected) in [
        (old, "at this build's versions"),
        (unevidenced, "cites no evidence"),
        (intent, "is not bindable"),
    ] {
        match records.write(&record, None) {
            Err(error @ ConformanceError::Refused { .. }) => {
                assert!(error.to_string().contains(expected), "{error}");
            }

            other => panic!("expected `{expected}`: {other:?}"),
        }

        assert!(!records.path(&record.declaration).exists());
    }
}

/// Records live outside what the store loads and scans (§8.1): writing
/// one beside an open project needs none of its locks and adopts no spec
/// revision.
#[test]
fn writing_a_record_adopts_no_spec_revision_and_takes_no_spec_lock() {
    let project = temp_dir("beside-the-store");
    let store = ProjectStore::create(&project, &fixture("flash_checkout.yaml")).unwrap();

    Records::of_project(&project)
        .write(&record(APPLY), None)
        .unwrap();

    assert!(store.disk_change().unwrap().is_none());

    drop(store);

    let (_, workspace) = ProjectStore::open(&project).unwrap();

    assert_eq!(
        Partition::of(&workspace).members[&DeclarationKey::parse(APPLY).unwrap()],
        record(APPLY).residual
    );
}

// ---------------------------------------------------------------------
// §14.4 Policy
// ---------------------------------------------------------------------

#[test]
fn without_a_policy_file_the_defaults_hold() {
    let policy = Policy::read(&temp_dir("policy-default")).unwrap();

    assert_eq!(policy, Policy::default());

    for kind in [
        "program",
        "transaction",
        "effect_site",
        "data_object",
        "outbox",
        "topic",
        "subscription_runtime",
        "outbox_runtime",
    ] {
        assert!(policy.requires_binding(kind), "{kind}");
    }

    assert!(!policy.requires_binding("service"));
    assert_eq!(policy.review.relied_on_facts, ReviewRule::Required);
    assert_eq!(policy.review.completeness, ReviewRule::Required);
    assert_eq!(policy.review.structural, ReviewRule::Optional);
    assert_eq!(policy.review.realization, ReviewRule::Required);
    assert_eq!(
        policy.review.checker_satisfies.iter().collect::<Vec<_>>(),
        vec!["realization"]
    );
}

#[test]
fn a_policy_file_overrides_only_what_it_states() {
    let project = temp_dir("policy-partial");

    put(
        &project,
        ".conseqa/conformance/policy.yaml",
        "format: 1\nrequire_binding: [transaction, router]\nreview:\n  structural: required\n",
    );

    let policy = Policy::read(&project).unwrap();

    assert_eq!(
        policy.require_binding.iter().collect::<Vec<_>>(),
        vec!["router", "transaction"]
    );
    assert_eq!(policy.review.structural, ReviewRule::Required);
    assert_eq!(policy.review.completeness, ReviewRule::Required);
    assert_eq!(policy.review.realization, ReviewRule::Required);
}

#[test]
fn a_policy_that_does_not_load_is_refused_by_name() {
    let cases = [
        ("require_binding: [transaction]\n", "missing `format`"),
        (
            "format: 2\nbind_everything: true\n",
            "is conformance format 2",
        ),
        ("format: 1\nowner: me\n", "unknown field `owner`"),
        (
            "format: 1\nrequire_binding: [requirement]\n",
            "lists `requirement`, which is not a bindable kind",
        ),
        (
            "format: 1\nrequire_binding: [prompt_obligation]\n",
            "lists `prompt_obligation`, which is not a bindable kind",
        ),
        (
            "format: 1\nreview:\n  checker_satisfies: [isolation]\n",
            "satisfies only `realization` facts",
        ),
        (
            "format: 1\nreview:\n  structural: sometimes\n",
            "unknown variant `sometimes`",
        ),
    ];

    for (text, expected) in cases {
        let project = temp_dir("policy-refused");

        put(&project, ".conseqa/conformance/policy.yaml", text);

        let message = Policy::read(&project).unwrap_err().to_string();

        assert!(
            message.contains(expected) && message.contains("policy.yaml"),
            "expected `{expected}`: {message}"
        );
    }
}
