//! Assembly of the self-contained HTML document.
//!
//! The front end is a Vite/React application in `viz/`, built into a
//! single `dist/index.html` with every script and stylesheet inlined.
//! This module embeds that bundle and injects the page data, so the
//! output opens from disk with no server and makes no external
//! requests. Rebuild the bundle with `npm run build` in `viz/` after
//! changing the front end; the built file is committed so `cargo`
//! needs no Node toolchain.

use crate::spec::Model;
use serde::Serialize;

use super::graph;
use crate::analyzer::report::ProverReport;

const BUNDLE: &str = include_str!("../../viz/dist/index.html");

const DATA_PLACEHOLDER: &str = "/*__CONSEQA_DATA__*/";
const TITLE_PLACEHOLDER: &str = "<title>conseqa</title>";

#[derive(Serialize)]
struct PageData<'a> {
    title: &'a str,
    model: &'a Model,
    graph: graph::Graph,
    report: Option<&'a ProverReport>,
}

/// The page data as pretty JSON, for the front end's development loop.
pub fn page_data_json(
    model: &Model,
    report: Option<&ProverReport>,
    title: &str,
) -> Result<String, String> {
    let data = PageData {
        title,
        model,
        graph: graph::extract(model),
        report,
    };

    serde_json::to_string_pretty(&data)
        .map_err(|error| format!("cannot serialize page data: {error}"))
}

pub fn render(model: &Model, report: Option<&ProverReport>, title: &str) -> Result<String, String> {
    let data = PageData {
        title,
        model,
        graph: graph::extract(model),
        report,
    };

    let json = serde_json::to_string(&data)
        .map_err(|error| format!("cannot serialize page data: {error}"))?;

    // `<` only occurs inside JSON string literals, where the escape
    // sequence `\u003c` is equivalent; this keeps `</script>`,
    // `<script`, and `<!--` (which would shift the parser into the
    // script-data escaped states) inert in the inline data block.
    let json = json.replace('<', "\\u003c");

    if !BUNDLE.contains(DATA_PLACEHOLDER) {
        return Err("the embedded front-end bundle lacks the page-data placeholder".to_string());
    }

    Ok(BUNDLE
        .replace(DATA_PLACEHOLDER, &format!("window.CONSEQA = {json};"))
        .replace(
            TITLE_PLACEHOLDER,
            &format!("<title>{} · conseqa</title>", escape_html(title)),
        ))
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_self_contained_html() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/flash_checkout.yaml");

        let model =
            crate::parser::yaml::parse(&std::fs::read_to_string(path).expect("fixture readable"))
                .expect("fixture parses");

        let html = render(&model, None, "smoke </script> test").expect("renders");

        // Placeholders substituted.
        assert!(!html.contains(DATA_PLACEHOLDER));
        assert!(!html.contains(TITLE_PLACEHOLDER));

        assert!(html.contains("window.CONSEQA"));
        assert!(html.contains("operation.create_order"));
        assert!(html.contains("<title>smoke &lt;/script&gt; test · conseqa</title>"));

        // No unescaped close tag may survive inside the data block:
        // the bundle's own script closers are the only ones present.
        assert_eq!(
            html.matches("</script>").count(),
            BUNDLE.matches("</script>").count()
        );
    }

    #[test]
    fn neutralizes_script_data_escape_sequences() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal.yaml");

        let mut model =
            crate::parser::yaml::parse(&std::fs::read_to_string(path).expect("fixture readable"))
                .expect("fixture parses");

        // A `<!--` followed by `<script` inside script data would put
        // the HTML parser into the double-escaped state, where the
        // template's real close tag no longer ends the element.
        if let Some(crate::spec::Schema::Canonical(schema)) = model.schemas.values_mut().next() {
            schema.description = Some("Beware <!-- of <script> tricks".to_string());
        } else {
            panic!("fixture has a canonical schema");
        }

        let html = render(&model, None, "hostile").expect("renders");

        assert!(!html.contains("of <script>"));
        assert!(html.contains("Beware \\u003c!-- of \\u003cscript"));
        assert_eq!(html.matches("<!--").count(), BUNDLE.matches("<!--").count());
        assert_eq!(
            html.matches("</script>").count(),
            BUNDLE.matches("</script>").count()
        );
    }
}
