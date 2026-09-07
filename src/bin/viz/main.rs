//! conseqa-viz: renders an Conseqa model as a self-contained,
//! interactive HTML presentation layer.
//!
//! The output is a single file with no external requests: system graph
//! (services, operations, topics, externals; publications,
//! subscriptions, and requests routing via topics), per-operation
//! program drill-down (steps → transactions → transitions), and
//! interactive state-machine graphs. The model checker's obligation
//! report (`conseqa::analyzer::report`) can be overlaid — computed
//! in-process with `--verify`, or loaded with `--report` — to mark
//! obligations proven, disproven, or unknown. The front end itself is
//! the React application in `viz/`, embedded as a built bundle.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use conseqa::analyzer::report;
use conseqa::analyzer::{Diagnostic, Severity};
use conseqa::viz::render;

struct Args {
    model: PathBuf,
    out: Option<PathBuf>,
    report: Option<PathBuf>,
    title: Option<String>,
    example_report: bool,
    verify: bool,
    json: bool,
    validate: bool,
}

const USAGE: &str = "\
conseqa-viz — interactive visualization for Conseqa models

USAGE:
    conseqa-viz <MODEL.yaml> [OPTIONS]

OPTIONS:
    --out <PATH>       Output path. Defaults to <MODEL>.html, or stdout
                       for --example-report.
    --report <PATH>    Prover report (JSON) to overlay on the model.
    --verify           Run the model checker and overlay its obligation
                       report, instead of reading one from --report.
    --json             Instead of rendering HTML, emit the page data
                       (title, model, graph, report) as JSON, for the
                       front end's development server.
    --title <TITLE>    Page title. Defaults to the model file name.
    --example-report   Instead of rendering, emit a scaffold prover
                       report enumerating every obligation implied by
                       the model's declared requirements, with every
                       status 'unknown'. Documents the report format
                       the visualization consumes.
    --no-validate      Skip analyzer validation warnings.
    -h, --help         Show this help.
";

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let source = std::fs::read_to_string(&args.model)
        .map_err(|error| format!("cannot read {}: {error}", args.model.display()))?;

    let model = conseqa::parser::yaml::parse(&source)
        .map_err(|error| format!("cannot parse {}: {error}", args.model.display()))?;

    if args.validate {
        for error in conseqa::analyzer::validate(&model) {
            report_diagnostic(&Diagnostic::from(error));
        }
    }

    if args.example_report {
        let scaffold = report::scaffold(&model);

        let json = serde_json::to_string_pretty(&scaffold)
            .map_err(|error| format!("cannot serialize report: {error}"))?;

        return match &args.out {
            Some(path) => write_output(path, &json),
            None => {
                println!("{json}");
                Ok(())
            }
        };
    }

    let prover_report = if args.verify {
        Some(report::obligations(
            &model,
            &conseqa::analyzer::verification::verify(&model),
        ))
    } else {
        match &args.report {
            Some(path) => {
                let raw = std::fs::read_to_string(path)
                    .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

                let parsed: report::ProverReport = serde_json::from_str(&raw)
                    .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;

                if let Some(revision) = parsed.model_revision
                    && revision != model.revision.0
                {
                    eprintln!(
                        "warning: report was produced against model \
                     revision {revision}, but the model is revision {}",
                        model.revision.0
                    );
                }

                Some(parsed)
            }
            None => None,
        }
    };

    let title = args.title.clone().unwrap_or_else(|| {
        args.model
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "conseqa model".to_string())
    });

    if args.json {
        let json = render::page_data_json(&model, prover_report.as_ref(), &title)?;

        return match &args.out {
            Some(path) => {
                write_output(path, &format!("{json}\n"))?;
                eprintln!("wrote {}", path.display());
                Ok(())
            }
            None => {
                println!("{json}");
                Ok(())
            }
        };
    }

    let html = render::render(&model, prover_report.as_ref(), &title)?;

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| default_out_path(&args.model));

    write_output(&out, &html)?;
    eprintln!("wrote {}", out.display());

    Ok(())
}

fn default_out_path(model: &Path) -> PathBuf {
    model.with_extension("html")
}

fn write_output(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn report_diagnostic(diagnostic: &Diagnostic) {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Unknown => "note",
    };

    let subject = diagnostic
        .subject
        .as_ref()
        .map(|id| format!(" [{id}]"))
        .unwrap_or_default();

    eprintln!(
        "model {severity}{subject}: {} (rendering anyway)",
        diagnostic.message
    );

    for evidence in &diagnostic.evidence {
        let subject = evidence
            .subject
            .as_ref()
            .map(|id| format!("[{id}] "))
            .unwrap_or_default();

        eprintln!("    {subject}{}", evidence.message);
    }
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut model = None;
    let mut out = None;
    let mut report = None;
    let mut title = None;
    let mut example_report = false;
    let mut verify = false;
    let mut json = false;
    let mut validate = true;

    let mut argv = std::env::args().skip(1);

    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--out" => {
                out = Some(PathBuf::from(expect_value(&arg, &mut argv)?));
            }
            "--report" => {
                report = Some(PathBuf::from(expect_value(&arg, &mut argv)?));
            }
            "--title" => {
                title = Some(expect_value(&arg, &mut argv)?);
            }
            "--example-report" => example_report = true,
            "--verify" => verify = true,
            "--json" => json = true,
            "--no-validate" => validate = false,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}"));
            }
            _ => {
                if model.replace(PathBuf::from(&arg)).is_some() {
                    return Err("more than one model path given".to_string());
                }
            }
        }
    }

    let Some(model) = model else {
        return Err("no model path given".to_string());
    };

    if verify && report.is_some() {
        return Err("--verify and --report are mutually exclusive".to_string());
    }

    Ok(Some(Args {
        model,
        out,
        report,
        title,
        example_report,
        verify,
        json,
        validate,
    }))
}

fn expect_value(flag: &str, argv: &mut impl Iterator<Item = String>) -> Result<String, String> {
    argv.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}
