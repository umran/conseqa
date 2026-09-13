use std::collections::BTreeMap;
use std::fmt;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::id::Id;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schema {
    Canonical(CanonicalSchema),
    Fragment(SchemaFragment),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSchema {
    /// Prose, carrying no semantic weight, so a declaration that has
    /// nothing to say may omit it rather than write an explicit null.
    #[serde(default)]
    pub description: Option<String>,

    pub completeness: SchemaCompleteness,
    pub fields: BTreeMap<String, Field>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaCompleteness {
    /// The declaration may omit fields that exist in the real schema.
    Partial,

    /// The declaration claims to describe the complete schema.
    Complete,
}

/// A field's logical type, and whether the logical value may be
/// absent.
///
/// The canonical form is a map:
///
/// ```yaml
/// order_id:
///   ty:
///     kind: scalar
///     value: uuid
///   optional: false
/// ```
///
/// The same declaration may be written as a shorthand string, where a
/// trailing `?` marks the field optional:
///
/// ```yaml
/// order_id: uuid
/// note: string?
/// customer: schema.Customer
/// items: [schema.LineItem]
/// tags: "[string]?"
/// ```
///
/// Compressing `optional` into a suffix is safe in a way that
/// defaulting most of this DSL would not be: optionality is a total
/// two-valued shape claim with no epistemic `unspecified` member
/// (§4), so the shorthand withholds no fact the long form states.
///
/// The shorthand is an authoring affordance only. Serialization
/// always emits the canonical map, because the serialized model is
/// also the wire format tooling reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    pub ty: TypeRef,
    pub optional: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename = "Field")]
struct FieldLong {
    ty: TypeRef,
    optional: bool,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(FieldVisitor)
    }
}

struct FieldVisitor;

impl<'de> Visitor<'de> for FieldVisitor {
    type Value = Field;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a field declaration: a shorthand type such as `uuid` or `string?`, \
             a one-element list such as `[string]`, \
             or a map with `ty` and `optional`",
        )
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let text = text.trim();

        let (ty, optional) = match text.strip_suffix('?') {
            Some(rest) => (rest, true),
            None => (text, false),
        };

        let ty = parse_type(ty).map_err(E::custom)?;

        Ok(Field { ty, optional })
    }

    fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        Ok(Field {
            ty: list_from_seq(seq)?,
            optional: false,
        })
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let long = FieldLong::deserialize(MapAccessDeserializer::new(map))?;

        Ok(Field {
            ty: long.ty,
            optional: long.optional,
        })
    }
}

/// A logical type.
///
/// Accepts the canonical tagged map, a shorthand name (`uuid` for a
/// scalar, anything else for a schema reference), or a one-element
/// sequence for a list (`[string]`). A trailing `?` belongs to the
/// field, not the type, so it is rejected here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TypeRef {
    Scalar(ScalarType),

    /// Reference to another declared schema.
    Schema(Id),

    List(Box<TypeRef>),
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    rename = "TypeRef"
)]
enum TypeRefLong {
    Scalar(ScalarType),
    Schema(Id),
    List(Box<TypeRef>),
}

impl From<TypeRefLong> for TypeRef {
    fn from(long: TypeRefLong) -> Self {
        match long {
            TypeRefLong::Scalar(scalar) => TypeRef::Scalar(scalar),
            TypeRefLong::Schema(schema) => TypeRef::Schema(schema),
            TypeRefLong::List(inner) => TypeRef::List(inner),
        }
    }
}

impl<'de> Deserialize<'de> for TypeRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TypeRefVisitor)
    }
}

struct TypeRefVisitor;

impl<'de> Visitor<'de> for TypeRefVisitor {
    type Value = TypeRef;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a type: a shorthand name such as `uuid` or `schema.Order`, \
             a one-element list such as `[string]`, \
             or a map with `kind` and `value`",
        )
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_type(text).map_err(E::custom)
    }

    fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        list_from_seq(seq)
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        TypeRefLong::deserialize(MapAccessDeserializer::new(map)).map(TypeRef::from)
    }
}

/// Reads the shorthand type grammar:
///
/// ```text
/// type := scalar-name | schema-id | "[" type "]"
/// ```
///
/// A name that matches a scalar is that scalar; every other name is a
/// schema reference. A schema whose id collides with a scalar name,
/// or ends in `?`, must be declared in the canonical map form.
fn parse_type(text: &str) -> Result<TypeRef, String> {
    let text = text.trim();

    if text.is_empty() {
        return Err("expected a type name".to_string());
    }

    if text.ends_with('?') {
        return Err(format!(
            "`{text}`: `?` marks a field optional and cannot appear inside a type"
        ));
    }

    if let Some(rest) = text.strip_prefix('[') {
        let inner = rest
            .strip_suffix(']')
            .ok_or_else(|| format!("`{text}`: unterminated `[` in list type"))?;

        return Ok(TypeRef::List(Box::new(parse_type(inner)?)));
    }

    if text.ends_with(']') {
        return Err(format!("`{text}`: unmatched `]` in list type"));
    }

    Ok(match ScalarType::from_name(text) {
        Some(scalar) => TypeRef::Scalar(scalar),
        None => TypeRef::Schema(Id(text.to_string())),
    })
}

fn list_from_seq<'de, A>(mut seq: A) -> Result<TypeRef, A::Error>
where
    A: SeqAccess<'de>,
{
    let Some(inner) = seq.next_element::<TypeRef>()? else {
        return Err(de::Error::custom(
            "a list shorthand holds exactly one element type, but none was given",
        ));
    };

    if seq.next_element::<IgnoredAny>()?.is_some() {
        return Err(de::Error::custom(
            "a list shorthand holds exactly one element type",
        ));
    }

    Ok(TypeRef::List(Box::new(inner)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarType {
    String,
    Bool,
    Int,
    Float,
    Decimal,
    Uuid,
    Timestamp,
}

impl ScalarType {
    /// The scalar named by a shorthand type, if the name is one.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "string" => ScalarType::String,
            "bool" => ScalarType::Bool,
            "int" => ScalarType::Int,
            "float" => ScalarType::Float,
            "decimal" => ScalarType::Decimal,
            "uuid" => ScalarType::Uuid,
            "timestamp" => ScalarType::Timestamp,
            _ => return None,
        })
    }
}

/// A path to a nested value, relative to a schema.
///
/// The canonical form is the sequence of components. A dotted string
/// says the same thing:
///
/// ```yaml
/// path: customer.id
///
/// path:
///   - customer
///   - id
/// ```
///
/// Dotted is already how a path is rendered back to the author —
/// `Display` joins with `.`, so diagnostics name `customer.id` — and
/// the shorthand only lets a declaration be written the way it will
/// be read. A component containing a `.` must use the sequence form.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct FieldPath(pub Vec<String>);

impl<'de> Deserialize<'de> for FieldPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(FieldPathVisitor)
    }
}

struct FieldPathVisitor;

impl<'de> Visitor<'de> for FieldPathVisitor {
    type Value = FieldPath;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a field path: a dotted name such as `customer.id`, \
             or a sequence of components",
        )
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let text = text.trim();

        if text.is_empty() {
            return Err(E::custom("expected a field path"));
        }

        let mut components = Vec::new();

        for component in text.split('.') {
            let component = component.trim();

            if component.is_empty() {
                return Err(E::custom(format!(
                    "`{text}`: a field path has no empty components"
                )));
            }

            components.push(component.to_string());
        }

        Ok(FieldPath(components))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut components = Vec::new();

        // An empty sequence stays parseable, so that a path naming
        // nothing keeps failing where it always has: in validation,
        // which resolves paths against their schema.
        while let Some(component) = seq.next_element::<String>()? {
            components.push(component);
        }

        Ok(FieldPath(components))
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("."))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaFragment {
    pub source: Id,
    pub mapping: BTreeMap<String, FieldPath>,
}
