//! The `sql_ddl` checker: a table of SQL migrations has a primary key or
//! a unique constraint over columns, is partitioned by columns, or has a
//! trigger.
//!
//! It reads the statements that declare those, as written in
//! migrations: `CREATE TABLE`, with column and table constraints and
//! `PARTITION BY`; `ALTER TABLE … ADD … PRIMARY KEY | UNIQUE`; `CREATE
//! UNIQUE INDEX … ON`; and `CREATE TRIGGER … ON`. Identifiers compare
//! without their quotes and case, and a schema-qualified table matches
//! its unqualified name.

use super::super::catalogue::SqlDdlAssert;
use super::{Finding, Read};

pub(super) fn check(assert: &SqlDdlAssert, regions: &[&Read<'_>]) -> Finding {
    let table = identifier(&assert.table);
    let mut declared = Table::default();

    for region in regions {
        for statement in statements(region.text) {
            declared.read(&statement, &table);
        }
    }

    if !declared.found {
        return Finding::disputed(format!(
            "no statement declares the table `{}`",
            assert.table
        ));
    }

    let columns = |columns: &[String]| -> Vec<String> {
        columns.iter().map(|column| identifier(column)).collect()
    };

    let as_set = |columns: &[String]| -> Vec<String> {
        let mut columns = columns.to_vec();
        columns.sort();
        columns
    };

    let (holds, what) = if let Some(primary) = &assert.primary_key {
        let wanted = as_set(&columns(primary));

        (
            declared
                .primary_keys
                .iter()
                .any(|key| as_set(key) == wanted),
            format!("a primary key over ({})", primary.join(", ")),
        )
    } else if let Some(unique) = &assert.unique {
        let wanted = as_set(&columns(unique));

        (
            declared
                .primary_keys
                .iter()
                .chain(&declared.uniques)
                .any(|key| as_set(key) == wanted),
            format!("a unique constraint over ({})", unique.join(", ")),
        )
    } else if let Some(partition) = &assert.partition_by {
        (
            declared.partitioned_by.as_deref() == Some(columns(partition).as_slice()),
            format!("PARTITION BY ({})", partition.join(", ")),
        )
    } else {
        (declared.triggers > 0, "a trigger".to_string())
    };

    if holds {
        Finding::confirmed(format!("`{}` has {what}", assert.table))
    } else {
        Finding::disputed(format!("`{}` has no {what}", assert.table))
    }
}

/// What the statements declare of one table.
#[derive(Debug, Default)]
struct Table {
    found: bool,
    primary_keys: Vec<Vec<String>>,
    uniques: Vec<Vec<String>>,
    partitioned_by: Option<Vec<String>>,
    triggers: usize,
}

impl Table {
    fn read(&mut self, statement: &str, table: &str) {
        let upper = statement.to_ascii_uppercase();
        let words: Vec<&str> = upper.split_whitespace().collect();

        if words.starts_with(&["CREATE", "TABLE"]) {
            let rest = after_words(statement, &["CREATE", "TABLE", "IF", "NOT", "EXISTS"]);
            let (name, rest) = name(rest);

            if identifier(name) != table {
                return;
            }

            self.found = true;

            let Some((body, after)) = parenthesized(rest) else {
                return;
            };

            for element in split_top_level(body) {
                self.element(&element);
            }

            let after_upper = after.to_ascii_uppercase();

            if let Some(at) = after_upper.find("PARTITION BY") {
                let rest = &after[at + "PARTITION BY".len()..];

                if let Some((columns, _)) = parenthesized(rest) {
                    self.partitioned_by = Some(column_list(columns));
                }
            }
        } else if words.starts_with(&["ALTER", "TABLE"]) {
            let rest = after_words(statement, &["ALTER", "TABLE", "ONLY", "IF", "EXISTS"]);
            let (name, rest) = name(rest);

            if identifier(name) == table {
                self.found = true;
                self.constraint(rest);
            }
        } else if words.starts_with(&["CREATE", "UNIQUE", "INDEX"])
            && let Some(at) = upper.find(" ON ")
        {
            let rest = after_words(&statement[at + 4..], &["ONLY"]);
            let (name, rest) = name(rest);

            if identifier(name) == table
                && let Some((columns, _)) = parenthesized(rest)
            {
                self.uniques.push(column_list(columns));
            }
        } else if words.first() == Some(&"CREATE")
            && words.contains(&"TRIGGER")
            && let Some(at) = upper.find(" ON ")
        {
            let rest = after_words(&statement[at + 4..], &["ONLY"]);
            let (name, _) = name(rest);

            if identifier(name) == table {
                self.triggers += 1;
            }
        }
    }

    /// One element of a `CREATE TABLE` body: a column with its
    /// constraints, or a table constraint.
    fn element(&mut self, element: &str) {
        let upper = element.to_ascii_uppercase();
        let trimmed = upper.trim_start();

        if trimmed.starts_with("PRIMARY KEY")
            || trimmed.starts_with("UNIQUE")
            || trimmed.starts_with("CONSTRAINT")
        {
            self.constraint(element);
            return;
        }

        let column = element.split_whitespace().next().unwrap_or_default();

        if upper.contains("PRIMARY KEY") {
            self.primary_keys.push(vec![identifier(column)]);
        } else if upper.split_whitespace().any(|word| word == "UNIQUE") {
            self.uniques.push(vec![identifier(column)]);
        }
    }

    /// A table constraint, named or not: `PRIMARY KEY (…)` or `UNIQUE
    /// (…)`.
    fn constraint(&mut self, text: &str) {
        let upper = text.to_ascii_uppercase();

        for (keyword, primary) in [("PRIMARY KEY", true), ("UNIQUE", false)] {
            let Some(at) = upper.find(keyword) else {
                continue;
            };

            if let Some((columns, _)) = parenthesized(&text[at + keyword.len()..]) {
                let columns = column_list(columns);

                if primary {
                    self.primary_keys.push(columns);
                } else {
                    self.uniques.push(columns);
                }

                return;
            }
        }
    }
}

/// The statements of SQL text, comments removed, each with its
/// whitespace collapsed.
fn statements(text: &str) -> Vec<String> {
    let mut cleaned = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = None;

    while let Some(c) = chars.next() {
        match (quoted, c) {
            (Some(quote), c) => {
                cleaned.push(c);

                if c == quote {
                    quoted = None;
                }
            }

            (None, '\'' | '"' | '`') => {
                quoted = Some(c);
                cleaned.push(c);
            }

            (None, '-') if chars.peek() == Some(&'-') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        cleaned.push('\n');
                        break;
                    }
                }
            }

            (None, '/') if chars.peek() == Some(&'*') => {
                chars.next();

                let mut previous = ' ';

                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }

                    previous = c;
                }

                cleaned.push(' ');
            }

            (None, c) => cleaned.push(c),
        }
    }

    cleaned
        .split(';')
        .map(|statement| statement.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// The text after the leading words, those of `words` it begins with,
/// in order and any case.
fn after_words<'t>(text: &'t str, words: &[&str]) -> &'t str {
    let mut rest = text.trim_start();

    for word in words {
        let next = rest.split_whitespace().next().unwrap_or_default();

        if next.eq_ignore_ascii_case(word) {
            rest = rest[next.len()..].trim_start();
        }
    }

    rest
}

/// The name that begins the text, up to whitespace or `(`, and the rest.
fn name(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(text.len());

    (&text[..end], &text[end..])
}

/// The contents of the first parenthesized group, and what follows it.
fn parenthesized(text: &str) -> Option<(&str, &str)> {
    let open = text.find('(')?;
    let mut depth = 0usize;

    for (index, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;

                if depth == 0 {
                    let close = open + index;

                    return Some((&text[open + 1..close], &text[close + 1..]));
                }
            }
            _ => {}
        }
    }

    None
}

/// The elements of a list, split at commas outside parentheses.
fn split_top_level(text: &str) -> Vec<String> {
    let mut elements = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();

    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                elements.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }

        current.push(c);
    }

    if !current.trim().is_empty() {
        elements.push(current.trim().to_string());
    }

    elements
}

fn column_list(text: &str) -> Vec<String> {
    split_top_level(text)
        .iter()
        .map(|column| identifier(column.split_whitespace().next().unwrap_or_default()))
        .collect()
}

/// An identifier without its quotes and case, and without its schema.
fn identifier(text: &str) -> String {
    let unquoted: String = text
        .trim()
        .chars()
        .filter(|c| !matches!(c, '"' | '`' | '[' | ']'))
        .collect();

    unquoted
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(sql: &str, name: &str) -> Table {
        let mut table = Table::default();

        for statement in statements(sql) {
            table.read(&statement, name);
        }

        table
    }

    #[test]
    fn keys_partitions_and_triggers_are_read_from_migrations() {
        let declared = table(
            "-- the orders\n\
             CREATE TABLE IF NOT EXISTS public.\"Orders\" (\n\
             \x20 order_id uuid PRIMARY KEY,\n\
             \x20 tenant_id uuid NOT NULL,\n\
             \x20 email text UNIQUE,\n\
             \x20 CONSTRAINT orders_tenant UNIQUE (tenant_id, order_id)\n\
             ) PARTITION BY HASH (tenant_id, order_id);\n\
             /* a trigger */\n\
             CREATE TRIGGER bump BEFORE UPDATE ON orders FOR EACH ROW EXECUTE FUNCTION bump();\n\
             ALTER TABLE ONLY orders ADD CONSTRAINT orders_ref UNIQUE (reference);\n\
             CREATE UNIQUE INDEX CONCURRENTLY orders_by_slug ON orders USING btree (slug);\n",
            "orders",
        );

        assert!(declared.found);
        assert_eq!(declared.primary_keys, [vec!["order_id".to_string()]]);
        assert!(declared.uniques.contains(&vec!["email".to_string()]));
        assert!(
            declared
                .uniques
                .contains(&vec!["tenant_id".to_string(), "order_id".to_string()])
        );
        assert!(declared.uniques.contains(&vec!["reference".to_string()]));
        assert!(declared.uniques.contains(&vec!["slug".to_string()]));
        assert_eq!(
            declared.partitioned_by,
            Some(vec!["tenant_id".to_string(), "order_id".to_string()])
        );
        assert_eq!(declared.triggers, 1);

        assert!(!table("CREATE TABLE payments (id uuid PRIMARY KEY);", "orders").found);
    }
}
