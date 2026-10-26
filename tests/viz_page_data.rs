//! The visualization front end's development data. `npm run data` runs
//! this ignored test, which writes `viz/public/conseqa.json` — the page
//! data for the video-streaming example, with the checker's report —
//! for `npm run dev` to serve.

use std::fs;
use std::path::PathBuf;

#[test]
#[ignore = "writes viz/public/conseqa.json; run it through `npm run data`"]
fn write_the_front_end_development_data() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let source = fs::read_to_string(root.join("tests/fixtures/video_streaming.yaml"))
        .expect("the example is readable");

    let model = conseqa::parser::yaml::parse(&source).expect("the example parses");

    assert!(
        conseqa::analyzer::validate(&model).is_empty(),
        "the example validates"
    );

    let report = conseqa::analyzer::report::obligations(
        &model,
        &conseqa::analyzer::verification::verify(&model),
    );

    let json = conseqa::viz::page_data_json(&model, Some(&report), "video_streaming")
        .expect("the page data serializes");

    let out = root.join("viz/public/conseqa.json");

    fs::create_dir_all(out.parent().expect("a parent")).expect("viz/public exists");
    fs::write(&out, format!("{json}\n")).expect("the page data is written");
}
