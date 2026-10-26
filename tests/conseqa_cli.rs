use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempProject(PathBuf);

impl TempProject {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("conseqa-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("create temporary project directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn conseqa(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_conseqa"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run conseqa")
}

#[test]
fn init_scaffolds_minimal_project_and_status_discovers_it_from_below() {
    let project = TempProject::new("init");
    let output = conseqa(project.path(), &["init"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(project.path().join(".conseqa/project.yaml").is_file());
    assert!(project.path().join(".conseqa/.gitignore").is_file());

    let nested = project.path().join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    let output = conseqa(&nested, &["status"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("head revision: 0"), "{stdout}");
    assert!(stdout.contains("commits:       0"), "{stdout}");
    assert!(stdout.contains("tasks:         0"), "{stdout}");
    let root = project.path().canonicalize().unwrap();
    assert!(
        stdout.contains(&format!("root:          {}", root.display())),
        "{stdout}"
    );
}

#[test]
fn status_outside_a_project_explains_how_to_initialize_one() {
    let outside = TempProject::new("outside");
    let output = conseqa(outside.path(), &["status"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("run `conseqa init`"), "{stderr}");
}

#[test]
fn init_refuses_to_create_a_nested_project() {
    let project = TempProject::new("nested");
    assert!(conseqa(project.path(), &["init"]).status.success());

    let nested = project.path().join("child");
    fs::create_dir(&nested).unwrap();
    let output = conseqa(&nested, &["init"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("inside the conseqa project"), "{stderr}");
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn succeeded(output: &Output) -> bool {
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    }

    output.status.success()
}

#[test]
fn an_adopted_model_is_checked_exported_and_visualized_headlessly() {
    let project = TempProject::new("adopt");

    assert!(succeeded(&conseqa(
        project.path(),
        &["init", "--model", &fixture("shop.yaml")],
    )));

    let manifest = fs::read_to_string(project.path().join(".conseqa/project.yaml")).unwrap();
    assert!(!manifest.contains("prompt"), "{manifest}");
    assert!(
        project
            .path()
            .join(".conseqa/spec/operations/operation.place_order.yaml")
            .is_file()
    );

    let output = conseqa(project.path(), &["check", "--report", "report.json"]);
    assert!(succeeded(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("obligations:"));
    assert!(project.path().join("report.json").is_file());

    assert!(succeeded(&conseqa(project.path(), &["export", "out"])));

    for artifact in ["conseqa.yaml", "verification-report.json", "spec.html"] {
        assert!(
            project.path().join("out").join(artifact).is_file(),
            "{artifact}"
        );
    }

    let exported = conseqa::parser::yaml::parse(
        &fs::read_to_string(project.path().join("out/conseqa.yaml")).unwrap(),
    )
    .expect("the export parses");
    let adopted =
        conseqa::parser::yaml::parse(&fs::read_to_string(fixture("shop.yaml")).unwrap()).unwrap();
    assert_eq!(exported.operations, adopted.operations);

    // From inside the project: the page lands in the uncommitted local
    // directory, and the page data goes to stdout.
    let nested = project.path().join("src");
    fs::create_dir_all(&nested).unwrap();

    assert!(succeeded(&conseqa(&nested, &["viz"])));
    assert!(project.path().join(".conseqa/local/spec.html").is_file());

    let output = conseqa(&nested, &["viz", "--json"]);
    assert!(succeeded(&output));

    let data: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the page data is JSON");
    let name = project.path().file_name().unwrap().to_string_lossy();
    assert_eq!(data["title"], *name, "titled with the project's name");
    assert!(data["report"].is_object(), "the verdicts are overlaid");
}

#[test]
fn check_fails_on_an_invalid_model_and_on_request_on_an_unproven_one() {
    let project = TempProject::new("check");

    assert!(succeeded(&conseqa(
        project.path(),
        &["init", "--model", &fixture("flash_checkout.yaml")],
    )));

    // The fixture leaves obligations unproven: a verdict, not a failure,
    // unless the caller requires every obligation proven.
    assert!(succeeded(&conseqa(project.path(), &["check"])));
    assert_eq!(
        conseqa(project.path(), &["check", "--require-proven"])
            .status
            .code(),
        Some(2)
    );

    // An operation of an undeclared service is a validation error.
    let operation = project
        .path()
        .join(".conseqa/spec/operations/operation.create_order.yaml");
    let text = fs::read_to_string(&operation).unwrap();

    fs::write(
        &operation,
        text.replace("service: service.checkout", "service: service.nowhere"),
    )
    .unwrap();

    let output = conseqa(project.path(), &["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("verification not attempted"), "{stderr}");

    // The page still renders, without verdicts.
    assert!(succeeded(&conseqa(project.path(), &["viz"])));
}

#[test]
fn design_needs_a_project_with_something_to_design() {
    let outside = TempProject::new("design-outside");
    let output = conseqa(outside.path(), &["design"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("run `conseqa init`"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let empty = TempProject::new("design-empty");
    assert!(succeeded(&conseqa(empty.path(), &["init"])));

    let output = conseqa(empty.path(), &["design"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("nothing to design"), "{stderr}");
}
