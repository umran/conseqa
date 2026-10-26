//! A model's delivery artifacts: the canonical `conseqa.yaml`, the
//! checker's `verification-report.json`, and the self-contained
//! `spec.html`. The MCP `export_spec` tool and `conseqa export` both
//! write them through [`write_artifacts`], so a directory either of
//! them writes always holds files describing one revision.

use std::path::{Path, PathBuf};

use crate::analyzer::report::ProverReport;
use crate::spec::Model;

pub const MODEL_FILE: &str = "conseqa.yaml";
pub const REPORT_FILE: &str = "verification-report.json";
pub const PAGE_FILE: &str = "spec.html";

/// Why an export stopped, and what it had written by then.
#[derive(Debug)]
pub struct ExportError {
    pub message: String,
    pub written: Vec<PathBuf>,
}

/// Writes `model`'s artifacts into `dir`, created if missing: the model
/// always, the report when there is one, and the page when `title` is
/// given. An artifact not written this time is removed if an earlier
/// export left one, so the report or page of an older revision never
/// sits beside a newer model.
pub fn write_artifacts(
    dir: &Path,
    model: &Model,
    report: Option<&ProverReport>,
    title: Option<&str>,
) -> Result<Vec<PathBuf>, ExportError> {
    let mut written = Vec::new();

    let fail = |message: String, written: &Vec<PathBuf>| ExportError {
        message,
        written: written.clone(),
    };

    std::fs::create_dir_all(dir).map_err(|error| {
        fail(
            format!("cannot create {}: {error}", dir.display()),
            &written,
        )
    })?;

    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    let yaml = serde_yaml::to_string(model)
        .map_err(|error| fail(format!("cannot serialize the model: {error}"), &written))?;

    write(&dir.join(MODEL_FILE), &yaml, &mut written)?;

    match report {
        Some(report) => {
            let json = serde_json::to_string_pretty(report)
                .map_err(|error| fail(format!("cannot serialize the report: {error}"), &written))?;

            write(&dir.join(REPORT_FILE), &format!("{json}\n"), &mut written)?;
        }

        None => remove_stale(&dir.join(REPORT_FILE), &written)?,
    }

    match title {
        Some(title) => {
            let html = crate::viz::render(model, report, title).map_err(|error| {
                fail(
                    format!("cannot render the visualization: {error}"),
                    &written,
                )
            })?;

            write(&dir.join(PAGE_FILE), &html, &mut written)?;
        }

        None => remove_stale(&dir.join(PAGE_FILE), &written)?,
    }

    Ok(written)
}

fn write(path: &Path, contents: &str, written: &mut Vec<PathBuf>) -> Result<(), ExportError> {
    std::fs::write(path, contents).map_err(|error| ExportError {
        message: format!("cannot write {}: {error}", path.display()),
        written: written.clone(),
    })?;

    written.push(path.to_path_buf());

    Ok(())
}

fn remove_stale(path: &Path, written: &[PathBuf]) -> Result<(), ExportError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ExportError {
            message: format!("cannot remove the stale {}: {error}", path.display()),
            written: written.to_vec(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        let source = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop.yaml"),
        )
        .expect("fixture readable");

        crate::parser::yaml::parse(&source).expect("fixture parses")
    }

    /// An export without a report or a page removes the ones an earlier
    /// export left, so the directory never mixes revisions.
    #[test]
    fn an_export_never_leaves_an_older_report_or_page_behind() {
        let dir = std::env::temp_dir().join(format!("conseqa-export-{}", uuid::Uuid::new_v4()));
        let model = model();

        let verification = crate::analyzer::verification::verify(&model);
        let report = crate::analyzer::report::obligations(&model, &verification);

        let first = write_artifacts(&dir, &model, Some(&report), Some("shop")).expect("exports");

        assert_eq!(first.len(), 3);
        assert!(dir.join(REPORT_FILE).is_file());
        assert!(dir.join(PAGE_FILE).is_file());

        let second = write_artifacts(&dir, &model, None, None).expect("exports");

        assert_eq!(second, vec![dir.canonicalize().unwrap().join(MODEL_FILE)]);
        assert!(!dir.join(REPORT_FILE).exists());
        assert!(!dir.join(PAGE_FILE).exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
