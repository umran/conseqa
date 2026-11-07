use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub String);

impl Id {
    /// The id's escaped spelling, as the project store names its file
    /// and as a textual declaration key writes it (§10.9 of the spec
    /// change analysis revision). Letters, digits, `_`, `-`, and `.`
    /// (not leading) are kept, and every other byte is `%XX`, so any id
    /// is a file name and a key segment holding neither `/` nor a
    /// space, and the common ids read as themselves.
    pub fn encoded(&self) -> String {
        let mut encoded = String::with_capacity(self.0.len());

        for (index, byte) in self.0.bytes().enumerate() {
            let kept = byte.is_ascii_alphanumeric()
                || byte == b'_'
                || byte == b'-'
                || (byte == b'.' && index > 0);

            if kept {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }

        encoded
    }

    /// The id an escaped spelling names, if the spelling is that id's
    /// canonical encoding. Any other spelling of the same bytes is
    /// refused, so no two spellings ever name one id.
    pub fn decode(encoded: &str) -> Option<Self> {
        let bytes = encoded.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;

        while index < bytes.len() {
            if bytes[index] == b'%' {
                let hex = encoded.get(index + 1..index + 3)?;

                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }

        let id = Self(String::from_utf8(decoded).ok()?);

        (!id.0.is_empty() && id.encoded() == encoded).then_some(id)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
