//! Reading the two sides of a change (§6 of the spec change analysis
//! revision; its tests §19.10), in temporary git repositories.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use conseqa::{
    analyzer::basis::DeclarationKey,
    design::{
        ProjectStore, RunId, RunMetadata, StoreError, WorkspaceState,
        change::{
            diff::{Change, declaration_changes},
            partition::Partition,
            side::{self, SideErrorReason, SideName, Source},
        },
    },
    parser::yaml,
    spec::Id,
};

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

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("conseqa-change-{label}-{}", uuid::Uuid::new_v4()));

    fs::create_dir_all(&dir).unwrap();

    dir
}

fn workspace() -> WorkspaceState {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flash_checkout.yaml");
    let model = yaml::parse(&fs::read_to_string(path).unwrap()).unwrap();

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("checkout".to_string())))
}

/// A repository holding the flash checkout project at `project`, a path
/// inside it, committed once.
fn repository(label: &str, project: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(label);

    git(&root, &["init", "-q"]);

    let project = root.join(project);

    fs::create_dir_all(&project).unwrap();

    drop(ProjectStore::create(&project, &workspace()).expect("the project is created"));

    commit(&root, "add the project");

    (root, project)
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", message]);
}

fn operation_file(project: &Path, operation: &str) -> PathBuf {
    project
        .join(".conseqa/spec/operations")
        .join(format!("{operation}.yaml"))
}

/// Makes `tx.apply_payment` serializable in the working tree.
fn strengthen_isolation(project: &Path) {
    let file = operation_file(project, "operation.apply_payment");
    let text = fs::read_to_string(&file).unwrap();

    assert!(text.contains("isolation: read_committed"));

    fs::write(
        &file,
        text.replacen("isolation: read_committed", "isolation: serializable", 1),
    )
    .unwrap();
}

fn read(project: &Path, name: SideName, source: Source) -> side::Side {
    side::read(project, name, &source).unwrap_or_else(|error| panic!("{error}"))
}

fn partition(side: &side::Side) -> Partition {
    side.workspace
        .as_ref()
        .map(Partition::of)
        .unwrap_or_default()
}

/// The one change strengthening the isolation makes.
fn assert_isolation_change(base: &side::Side, head: &side::Side) {
    let changes = declaration_changes(&partition(base), &partition(head));

    assert!(changes.authoring.is_empty(), "{changes:#?}");
    assert_eq!(changes.changes.len(), 1, "{changes:#?}");

    let change = &changes.changes[0];

    assert_eq!(
        change.declaration,
        DeclarationKey::Transaction {
            operation: Id("operation.apply_payment".to_string()),
            transaction: Id("tx.apply_payment".to_string()),
        }
    );
    assert_eq!(change.change, Change::Modified);
    assert_eq!(
        change
            .delta
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["isolation"]
    );
}

#[test]
fn the_working_tree_is_read_against_head() {
    let (_, project) = repository("working-tree", "");

    strengthen_isolation(&project);

    let base = read(&project, SideName::Base, Source::Commit("HEAD".to_string()));
    let head = read(&project, SideName::Head, Source::WorkingTree);

    assert!(base.commit.is_some() && head.commit.is_none());
    assert_isolation_change(&base, &head);
}

#[test]
fn two_commits_and_their_merge_base() {
    let (root, project) = repository("commits", "");

    let first = git(&root, &["rev-parse", "HEAD"]).trim().to_string();

    // The feature branch changes the isolation; master meanwhile
    // describes an operation, an authoring change.
    git(&root, &["switch", "-q", "-c", "feature"]);
    strengthen_isolation(&project);
    commit(&root, "strengthen the isolation");

    git(&root, &["switch", "-q", "master"]);

    let file = project.join(".conseqa/spec/schemas");
    let schema = fs::read_dir(&file).unwrap().next().unwrap().unwrap().path();
    let text = fs::read_to_string(&schema).unwrap();

    fs::write(
        &schema,
        text.replacen("description: null", "description: a record", 1),
    )
    .unwrap();
    commit(&root, "describe a schema");

    let base = read(&project, SideName::Base, Source::Commit(first.clone()));
    let head = read(
        &project,
        SideName::Head,
        Source::Commit("feature".to_string()),
    );

    assert_eq!(base.commit.as_deref(), Some(first.as_str()));
    assert_isolation_change(&base, &head);

    // `--merge-base` reads from where the branch left master, so what
    // master did since is not part of the branch's change.
    let merge_base = side::merge_base(&project, "master", &Source::Commit("feature".to_string()))
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(merge_base, first);

    let master = read(
        &project,
        SideName::Base,
        Source::Commit("master".to_string()),
    );
    let changes = declaration_changes(&partition(&master), &partition(&head));

    assert_eq!(
        changes.authoring.len(),
        1,
        "master's description shows without it"
    );
}

#[test]
fn a_commit_without_the_project_reads_as_empty() {
    let root = temp_dir("empty");

    git(&root, &["init", "-q"]);
    fs::write(root.join("README.md"), "before the project\n").unwrap();
    commit(&root, "start");

    drop(ProjectStore::create(&root, &workspace()).unwrap());
    commit(&root, "add the project");

    let base = read(&root, SideName::Base, Source::Commit("HEAD~1".to_string()));
    let head = read(&root, SideName::Head, Source::Commit("HEAD".to_string()));

    assert!(base.is_empty() && !head.is_empty());

    let head_partition = partition(&head);
    let changes = declaration_changes(&Partition::default(), &head_partition);

    assert_eq!(changes.changes.len(), head_partition.members.len());
    assert!(
        changes
            .changes
            .iter()
            .all(|change| change.change == Change::Added)
    );
}

#[test]
fn a_side_of_another_format_or_dsl_is_refused_naming_the_side() {
    let (root, project) = repository("versions", "");
    let file = project.join(".conseqa/project.yaml");
    let text = fs::read_to_string(&file).unwrap();

    fs::write(&file, text.replacen("format: 10", "format: 9", 1)).unwrap();
    commit(&root, "an older format");

    let error = side::read(
        &project,
        SideName::Base,
        &Source::Commit("HEAD".to_string()),
    )
    .expect_err("refused");

    assert!(matches!(
        error.reason,
        SideErrorReason::Store(StoreError::FormatMismatch { found: 9, .. })
    ));

    let message = error.to_string();

    assert!(message.starts_with("base (HEAD): "), "{message}");
    assert!(
        message.contains("format 9") && message.contains("format 10"),
        "{message}"
    );

    fs::write(&file, text.replacen("dsl: 6", "dsl: 5", 1)).unwrap();

    let error = side::read(&project, SideName::Head, &Source::WorkingTree).expect_err("refused");
    let message = error.to_string();

    assert!(matches!(
        error.reason,
        SideErrorReason::Store(StoreError::DslMismatch { .. })
    ));
    assert!(message.starts_with("head (working tree): "), "{message}");
    assert!(
        message.contains("dsl 5") && message.contains("dsl 6"),
        "{message}"
    );
}

#[test]
fn an_unresolvable_ref_is_refused() {
    let (_, project) = repository("unresolved", "");

    let error = side::read(
        &project,
        SideName::Base,
        &Source::Commit("no-such-branch".to_string()),
    )
    .expect_err("refused");

    assert!(matches!(
        &error.reason,
        SideErrorReason::UnresolvedRef { rev } if rev == "no-such-branch"
    ));
    assert!(
        error
            .to_string()
            .contains("`no-such-branch` names no commit")
    );
}

#[test]
fn a_broken_file_on_the_working_tree_is_refused_naming_it() {
    let (_, project) = repository("broken", "");
    let file = operation_file(&project, "operation.apply_payment");

    fs::write(&file, "service: [unclosed\n").unwrap();

    let error = side::read(&project, SideName::Head, &Source::WorkingTree).expect_err("refused");

    assert!(matches!(
        &error.reason,
        SideErrorReason::Store(StoreError::Parse { path, .. }) if path.ends_with("operation.apply_payment.yaml")
    ));
    assert!(error.to_string().contains("operation.apply_payment.yaml"));

    // The committed side is untouched by the edit.
    read(&project, SideName::Base, Source::Commit("HEAD".to_string()));
}

/// Every file of a directory, with its contents.
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

/// Reading a side writes nothing, touches no index, and takes no lock:
/// it reads while the project is open for writing elsewhere.
#[test]
fn reading_a_side_writes_nothing_and_takes_no_lock() {
    let (root, project) = repository("read-only", "");

    strengthen_isolation(&project);

    let (_held, _) = ProjectStore::open(&project).expect("the project opens for writing");

    let before = snapshot(&root);

    let base = read(&project, SideName::Base, Source::Commit("HEAD".to_string()));
    let head = read(&project, SideName::Head, Source::WorkingTree);

    side::merge_base(&project, "HEAD", &Source::WorkingTree).unwrap();
    side::read(
        &project,
        SideName::Base,
        &Source::Commit("missing".to_string()),
    )
    .unwrap_err();

    assert_isolation_change(&base, &head);
    assert_eq!(snapshot(&root), before, "reading changed a file");
}

#[test]
fn a_project_below_the_repository_root_is_read_at_its_path() {
    let (_, project) = repository("nested", "services/orders");

    let base = read(&project, SideName::Base, Source::Commit("HEAD".to_string()));
    let head = read(&project, SideName::Head, Source::WorkingTree);

    assert!(!base.is_empty());
    assert!(
        declaration_changes(&partition(&base), &partition(&head))
            .changes
            .is_empty()
    );
}

#[test]
fn a_git_side_outside_a_repository_is_refused() {
    let project = temp_dir("no-repository");

    drop(ProjectStore::create(&project, &workspace()).unwrap());

    let error = side::read(
        &project,
        SideName::Base,
        &Source::Commit("HEAD".to_string()),
    )
    .expect_err("refused");

    assert!(matches!(
        error.reason,
        SideErrorReason::NotARepository { .. }
    ));

    read(&project, SideName::Head, Source::WorkingTree);
}

/// A git tree records a symbolic link as a link, and the store reads
/// none: it is an unexpected entry, refused by name.
#[cfg(unix)]
#[test]
fn a_committed_symbolic_link_is_refused_by_name() {
    let (root, project) = repository("symlink", "");
    let link = operation_file(&project, "operation.alias");

    std::os::unix::fs::symlink("operation.apply_payment.yaml", &link).unwrap();
    commit(&root, "link an operation");

    let error = side::read(
        &project,
        SideName::Base,
        &Source::Commit("HEAD".to_string()),
    )
    .expect_err("refused");

    assert!(matches!(
        &error.reason,
        SideErrorReason::Store(StoreError::Unexpected { path, .. }) if path.ends_with("operation.alias.yaml")
    ));
}
