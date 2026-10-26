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
