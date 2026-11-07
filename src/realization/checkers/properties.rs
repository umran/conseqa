//! The `properties` checker: a key of Java-style `.properties` or
//! `key=value` client configuration equals a value.

use super::super::catalogue::PropertiesAssert;
use super::{Finding, Read};

pub(super) fn check(assert: &PropertiesAssert, regions: &[&Read<'_>]) -> Finding {
    let values: Vec<(String, &str)> = regions
        .iter()
        .flat_map(|region| {
            entries(region.text)
                .into_iter()
                .filter(|(key, _)| key == &assert.key)
                .map(move |(_, value)| (value, region.path))
        })
        .collect();

    if values.is_empty() {
        return Finding::disputed(format!("`{}` is not set", assert.key));
    }

    match values.iter().find(|(value, _)| *value != assert.equals.0) {
        Some((value, path)) => Finding::disputed(format!(
            "`{}` is `{value}` in {path}, not `{}`",
            assert.key, assert.equals.0
        )),
        None => Finding::confirmed(format!("`{}` is `{}`", assert.key, assert.equals.0)),
    }
}

/// The key-value entries of configuration text: one per line, split at
/// the first `=` or `:`, with `#` and `!` comment lines left out.
fn entries(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .filter_map(|line| {
            let split = line
                .find(['=', ':'])
                .or_else(|| line.find(char::is_whitespace))?;

            Some((
                line[..split].trim().to_string(),
                line[split + 1..].trim().to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_split_at_the_first_separator() {
        assert_eq!(
            entries(
                "# a comment\n! another\nacks = all\nenable.idempotence=true\nurl: http://x:9\n"
            ),
            [
                ("acks".to_string(), "all".to_string()),
                ("enable.idempotence".to_string(), "true".to_string()),
                ("url".to_string(), "http://x:9".to_string()),
            ]
        );
    }
}
