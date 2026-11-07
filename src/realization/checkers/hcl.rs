//! The `hcl` checker: a Terraform resource of a type exists, or an
//! attribute of every such resource equals, or contains, a value.
//!
//! It reads only what an assertion needs of HCL: `resource "<type>"
//! "<name>" { … }` blocks and their top-level `name = value` attributes.
//! Nested blocks are skipped whole, and comments and strings are read
//! as HCL reads them, so a brace inside either ends nothing.

use super::super::catalogue::HclAssert;
use super::{Finding, Read};

pub(super) fn check(assert: &HclAssert, regions: &[&Read<'_>]) -> Finding {
    let resources: Vec<(Resource, &str)> = regions
        .iter()
        .flat_map(|region| {
            resources(region.text)
                .into_iter()
                .filter(|resource| resource.kind == assert.resource)
                .map(move |resource| (resource, region.path))
        })
        .collect();

    if resources.is_empty() {
        return Finding::disputed(format!("no `{}` resource is declared", assert.resource));
    }

    let Some(attribute) = &assert.attribute else {
        return Finding::confirmed(format!(
            "`{}` resource `{}` is declared",
            assert.resource, resources[0].0.name
        ));
    };

    for (resource, path) in &resources {
        let Some(value) = resource.attribute(attribute) else {
            return Finding::disputed(format!(
                "`{}.{}` in {path} sets no `{attribute}`",
                assert.resource, resource.name
            ));
        };

        let holds = match (&assert.equals, &assert.contains) {
            (Some(equals), _) => unquoted(value) == equals.0,
            (None, Some(contains)) => contained(value, &contains.0),
            (None, None) => unreachable!("an attribute comes with equals or contains"),
        };

        if !holds {
            return Finding::disputed(format!(
                "`{}.{}` in {path} sets `{attribute} = {value}`",
                assert.resource, resource.name
            ));
        }
    }

    Finding::confirmed(format!(
        "every `{}` resource sets `{attribute}` as asserted",
        assert.resource
    ))
}

/// One `resource` block: its type, its name, and its top-level
/// attributes, each value as written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resource {
    kind: String,
    name: String,
    attributes: Vec<(String, String)>,
}

impl Resource {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(attribute, _)| attribute == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A string's contents, or a value as written.
fn unquoted(value: &str) -> String {
    let value = value.trim();

    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map_or_else(|| value.to_string(), str::to_string)
}

/// Whether a list holds an element, or a string holds a substring.
fn contained(value: &str, needle: &str) -> bool {
    let value = value.trim();

    match value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        Some(elements) => elements
            .split(',')
            .any(|element| unquoted(element) == needle),
        None => unquoted(value).contains(needle),
    }
}

/// HCL's tokens, as far as resources and attributes need them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Text(String),
    Open,
    Close,
    Equals,
    Newline,
    Other(char),
}

fn tokens(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < chars.len() {
        let c = chars[index];

        match c {
            '#' => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }

            '/' if chars.get(index + 1) == Some(&'/') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }

            '/' if chars.get(index + 1) == Some(&'*') => {
                index += 2;

                while index < chars.len()
                    && !(chars[index] == '*' && chars.get(index + 1) == Some(&'/'))
                {
                    index += 1;
                }

                index += 2;
            }

            '"' => {
                let mut value = String::from('"');
                index += 1;

                while index < chars.len() && chars[index] != '"' {
                    if chars[index] == '\\' && index + 1 < chars.len() {
                        value.push(chars[index]);
                        index += 1;
                    }

                    value.push(chars[index]);
                    index += 1;
                }

                value.push('"');
                tokens.push(Token::Text(value));
                index += 1;
            }

            '{' => {
                tokens.push(Token::Open);
                index += 1;
            }

            '}' => {
                tokens.push(Token::Close);
                index += 1;
            }

            '=' if chars.get(index + 1) != Some(&'=') => {
                tokens.push(Token::Equals);
                index += 1;
            }

            '\n' => {
                tokens.push(Token::Newline);
                index += 1;
            }

            c if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' => {
                let start = index;

                while index < chars.len()
                    && (chars[index].is_alphanumeric() || matches!(chars[index], '_' | '-' | '.'))
                {
                    index += 1;
                }

                tokens.push(Token::Word(chars[start..index].iter().collect()));
            }

            c if c.is_whitespace() => index += 1,

            c => {
                tokens.push(Token::Other(c));
                index += 1;
            }
        }
    }

    tokens
}

/// Every `resource` block of a region.
fn resources(text: &str) -> Vec<Resource> {
    let tokens = tokens(text);
    let mut resources = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        let block = match (
            &tokens[index],
            tokens.get(index + 1),
            tokens.get(index + 2),
            tokens.get(index + 3),
        ) {
            (
                Token::Word(word),
                Some(Token::Text(kind)),
                Some(Token::Text(name)),
                Some(Token::Open),
            ) if word == "resource" => Some((unquoted(kind), unquoted(name))),
            _ => None,
        };

        let Some((kind, name)) = block else {
            index += 1;
            continue;
        };

        let (attributes, end) = body(&tokens, index + 4);

        resources.push(Resource {
            kind,
            name,
            attributes,
        });

        index = end;
    }

    resources
}

/// A block body's top-level attributes, from just after its `{`, and
/// the index just after its `}`.
fn body(tokens: &[Token], mut index: usize) -> (Vec<(String, String)>, usize) {
    let mut attributes = Vec::new();

    while index < tokens.len() {
        match &tokens[index] {
            Token::Close => return (attributes, index + 1),

            Token::Word(name) if tokens.get(index + 1) == Some(&Token::Equals) => {
                let mut value = String::new();
                let mut depth = 0usize;

                index += 2;

                while index < tokens.len() {
                    match &tokens[index] {
                        Token::Newline if depth == 0 => break,
                        Token::Close if depth == 0 => break,
                        Token::Open => depth += 1,
                        Token::Close => depth -= 1,
                        _ => {}
                    }

                    value.push_str(&match &tokens[index] {
                        Token::Word(word) => word.clone(),
                        Token::Text(text) => text.clone(),
                        Token::Open => "{".to_string(),
                        Token::Close => "}".to_string(),
                        Token::Equals => "=".to_string(),
                        Token::Newline => " ".to_string(),
                        Token::Other(c) => c.to_string(),
                    });

                    index += 1;
                }

                attributes.push((name.clone(), value.trim().to_string()));
            }

            // A nested block, labelled or not: skipped whole.
            Token::Open => {
                let mut depth = 1usize;
                index += 1;

                while index < tokens.len() && depth > 0 {
                    match tokens[index] {
                        Token::Open => depth += 1,
                        Token::Close => depth -= 1,
                        _ => {}
                    }

                    index += 1;
                }
            }

            _ => index += 1,
        }
    }

    (attributes, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_and_their_top_level_attributes_are_read() {
        let found = resources(
            "resource \"kafka_topic\" \"orders\" {\n\
             \x20 name       = \"orders\" # the topic\n\
             \x20 partitions = 12\n\
             \x20 config = {\n\
             \x20   \"cleanup.policy\" = \"compact\"\n\
             \x20 }\n\
             \x20 lifecycle {\n\
             \x20   prevent_destroy = true\n\
             \x20 }\n\
             \x20 tags = [\"a\", \"b\"]\n\
             }\n\
             /* resource \"kafka_topic\" \"commented\" { partitions = 1 } */\n",
        );

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "orders");
        assert_eq!(found[0].attribute("partitions"), Some("12"));
        assert_eq!(found[0].attribute("name"), Some("\"orders\""));
        assert_eq!(found[0].attribute("prevent_destroy"), None);
        assert!(contained(found[0].attribute("tags").unwrap(), "b"));
    }
}
