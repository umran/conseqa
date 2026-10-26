use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// BLAKE3 hash of the deterministic serialization of semantic content.
///
/// Conseqa's spec types serialize deterministically — ordered maps,
/// fixed field order, tagged enums — so canonical JSON is a sufficient
/// fingerprint input. Hashes are runtime metadata for optimistic
/// concurrency and invalidation, not permanent external identities: a
/// serialization change may legitimately change every fingerprint.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SemanticHash([u8; 32]);

impl SemanticHash {
    /// Fingerprints one piece of semantic content through its
    /// canonical JSON serialization.
    pub fn of<T: Serialize + ?Sized>(value: &T) -> Self {
        let bytes = serde_json::to_vec(value).expect("semantic content serializes to JSON");

        Self(*blake3::hash(&bytes).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);

        for byte in self.0 {
            use fmt::Write;

            write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        }

        out
    }

    pub fn from_hex(text: &str) -> Result<Self, String> {
        let text = text.trim();

        if text.len() != 64 {
            return Err(format!(
                "a semantic hash is 64 hex characters, got {}",
                text.len()
            ));
        }

        let mut bytes = [0u8; 32];

        for (index, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| "invalid hex".to_string())?;

            bytes[index] = u8::from_str_radix(pair, 16)
                .map_err(|_| format!("`{pair}` is not a hex byte"))?;
        }

        Ok(Self(bytes))
    }
}

/// Renders the short prefix used in diagnostics; the full hash is the
/// serialized form.
impl fmt::Display for SemanticHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0[..6] {
            write!(f, "{byte:02x}")?;
        }

        f.write_str("…")
    }
}

impl fmt::Debug for SemanticHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SemanticHash({})", self.to_hex())
    }
}

impl Serialize for SemanticHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for SemanticHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(SemanticHashVisitor)
    }
}

struct SemanticHashVisitor;

impl Visitor<'_> for SemanticHashVisitor {
    type Value = SemanticHash;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a 64-character hex semantic hash")
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        SemanticHash::from_hex(text).map_err(E::custom)
    }
}
