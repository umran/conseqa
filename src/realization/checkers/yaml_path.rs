//! The `yaml_path` checker: a path of YAML or JSON configuration exists,
//! or its value equals one value or is one of several.
//!
//! A path is dotted keys, matched at any depth and crossing lists, so
//! that `lb_policy` finds an Envoy cluster's policy whether the region
//! holds the cluster alone or the whole bootstrap, and `spec.replicas`
//! finds a Kubernetes deployment's.

use serde_yaml::Value;

use super::super::catalogue::{Scalar, YamlPathAssert};
use super::{Finding, Read, dedent};

pub(super) fn check(assert: &YamlPathAssert, regions: &[&Read<'_>]) -> Finding {
    let path: Vec<&str> = assert.path.split('.').collect();
    let mut found: Vec<(Value, &str)> = Vec::new();

    for region in regions {
        let value: Value = match serde_yaml::from_str(&dedent(region.text)) {
            Ok(value) => value,
            Err(error) => {
                return Finding::disputed(format!(
                    "{} does not read as YAML or JSON: {error}",
                    region.path
                ));
            }
        };

        let mut values = Vec::new();
        descend(&value, &path, &mut values);

        found.extend(values.into_iter().map(|value| (value.clone(), region.path)));
    }

    if found.is_empty() {
        return Finding::disputed(format!("`{}` is not set", assert.path));
    }

    if assert.exists == Some(true) {
        return Finding::confirmed(format!("`{}` is set", assert.path));
    }

    let accepted: Vec<&Scalar> = match (&assert.equals, &assert.one_of) {
        (Some(equals), _) => vec![equals],
        (None, Some(one_of)) => one_of.iter().collect(),
        (None, None) => unreachable!("the reader requires exists, equals, or one_of"),
    };

    let accepts = |value: &Value| {
        text(value).is_some_and(|text| accepted.iter().any(|scalar| scalar.0 == text))
    };

    match found.iter().find(|(value, _)| !accepts(value)) {
        Some((value, path)) => Finding::disputed(format!(
            "`{}` is {} in {path}, not {}",
            assert.path,
            text(value).map_or_else(|| "not a scalar".to_string(), |text| format!("`{text}`")),
            accepted
                .iter()
                .map(|scalar| format!("`{}`", scalar.0))
                .collect::<Vec<_>>()
                .join(" or ")
        )),
        None => Finding::confirmed(format!(
            "`{}` is {}",
            assert.path,
            found
                .iter()
                .filter_map(|(value, _)| text(value))
                .map(|text| format!("`{text}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Every value at `path`, starting from any mapping at any depth.
fn descend<'v>(value: &'v Value, path: &[&str], found: &mut Vec<&'v Value>) {
    match value {
        Value::Mapping(mapping) => {
            follow(value, path, found);

            for child in mapping.values() {
                descend(child, path, found);
            }
        }

        Value::Sequence(items) => {
            for item in items {
                descend(item, path, found);
            }
        }

        Value::Tagged(tagged) => descend(&tagged.value, path, found),

        _ => {}
    }
}

/// The values at `path` from this one, crossing lists.
fn follow<'v>(value: &'v Value, path: &[&str], found: &mut Vec<&'v Value>) {
    let Some((first, rest)) = path.split_first() else {
        found.push(value);
        return;
    };

    match value {
        Value::Mapping(mapping) => {
            if let Some(child) = mapping.get(*first) {
                follow(child, rest, found);
            }
        }

        Value::Sequence(items) => {
            for item in items {
                follow(item, path, found);
            }
        }

        _ => {}
    }
}

/// A scalar value as written: a string, a boolean, or a number.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Tagged(tagged) => text(&tagged.value),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) => None,
    }
}
