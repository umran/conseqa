//! The `pattern` checker: a regular expression matches. The weakest
//! kind, for idioms whose evidence has no structured shape.

use regex::Regex;

use super::super::catalogue::PatternAssert;
use super::{Finding, Read};

pub(super) fn check(assert: &PatternAssert, regions: &[&Read<'_>]) -> Finding {
    let Ok(regex) = Regex::new(&assert.regex) else {
        return Finding::disputed(format!("`{}` does not compile", assert.regex));
    };

    match regions.iter().find(|region| regex.is_match(region.text)) {
        Some(region) => {
            Finding::confirmed(format!("`{}` matches in {}", assert.regex, region.path))
        }
        None => Finding::disputed(format!("`{}` matches in no region", assert.regex)),
    }
}
