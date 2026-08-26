use crate::error::{CoreError, ErrorCode, Result};
use crate::model::SkillRank;
use indexmap::IndexMap;
use std::ops::Range;

const MAX_SKILL_MEMBERS: usize = 16_384;
const MAX_SKILL_ID_BYTES: usize = 1_024;

#[derive(Debug, Clone)]
pub(crate) struct SkillJsonEntry {
    pub rank: SkillRank,
    pub value_range: Range<usize>,
}

/// A strict index over the saved JSON skill object. Ranges are relative to the
/// XML element's raw inner byte span so edits can patch only rank tokens or add
/// members without serializing existing data.
#[derive(Debug, Clone)]
pub(crate) struct SkillJsonDocument {
    entries: IndexMap<String, SkillJsonEntry>,
    insertion_offset: usize,
}

impl SkillJsonDocument {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        std::str::from_utf8(bytes)
            .map_err(|_| CoreError::validation("skill JSON is not valid UTF-8"))?;
        let mut parser = Parser { bytes, cursor: 0 };
        parser.skip_whitespace();
        parser.expect(b'{', "skill JSON must begin with an object")?;
        parser.skip_whitespace();

        let mut entries = IndexMap::new();
        let mut insertion_offset = parser.cursor;
        if parser.peek() == Some(b'}') {
            parser.cursor += 1;
            parser.finish()?;
            return Ok(Self {
                entries,
                insertion_offset,
            });
        }

        loop {
            if entries.len() >= MAX_SKILL_MEMBERS {
                return Err(CoreError::new(
                    ErrorCode::ResourceLimit,
                    "skill member limit exceeded",
                ));
            }
            let skill_id = parser.string()?;
            if skill_id.is_empty() || skill_id.len() > MAX_SKILL_ID_BYTES {
                return Err(CoreError::validation("invalid skill ID length"));
            }
            if entries.contains_key(&skill_id) {
                return Err(CoreError::ambiguous(format!(
                    "duplicate skill JSON member '{skill_id}'"
                )));
            }
            parser.skip_whitespace();
            parser.expect(b':', "skill JSON member is missing ':'")?;
            parser.skip_whitespace();
            let value_start = parser.cursor;
            let numeric = match parser.peek() {
                Some(value @ b'0'..=b'2') => {
                    parser.cursor += 1;
                    value - b'0'
                }
                _ => return Err(CoreError::validation("skill rank must be 0, 1, or 2")),
            };
            if parser.peek().is_some_and(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-')
            }) {
                return Err(CoreError::validation(
                    "skill rank must be an integer 0, 1, or 2",
                ));
            }
            let value_end = parser.cursor;
            insertion_offset = value_end;
            entries.insert(
                skill_id,
                SkillJsonEntry {
                    rank: SkillRank::from_numeric(numeric)?,
                    value_range: value_start..value_end,
                },
            );
            parser.skip_whitespace();
            match parser.peek() {
                Some(b',') => {
                    parser.cursor += 1;
                    parser.skip_whitespace();
                    if parser.peek() == Some(b'}') {
                        return Err(CoreError::validation(
                            "trailing commas are not allowed in saved skill JSON",
                        ));
                    }
                }
                Some(b'}') => {
                    parser.cursor += 1;
                    parser.finish()?;
                    break;
                }
                _ => {
                    return Err(CoreError::validation(
                        "skill JSON members must be separated by ','",
                    ));
                }
            }
        }

        Ok(Self {
            entries,
            insertion_offset,
        })
    }

    pub fn entries(&self) -> &IndexMap<String, SkillJsonEntry> {
        &self.entries
    }

    pub fn to_rank_map(&self) -> IndexMap<String, SkillRank> {
        self.entries
            .iter()
            .map(|(id, entry)| (id.clone(), entry.rank))
            .collect()
    }

    pub fn insertion_offset(&self) -> usize {
        self.insertion_offset
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.cursor += 1;
        }
    }

    fn expect(&mut self, byte: u8, message: &str) -> Result<()> {
        if self.peek() != Some(byte) {
            return Err(CoreError::validation(message));
        }
        self.cursor += 1;
        Ok(())
    }

    fn string(&mut self) -> Result<String> {
        let start = self.cursor;
        self.expect(b'"', "skill ID must be a JSON string")?;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            if byte < 0x20 {
                return Err(CoreError::validation(
                    "skill ID contains an unescaped control byte",
                ));
            }
            self.cursor += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return serde_json::from_slice(&self.bytes[start..self.cursor]).map_err(|error| {
                    CoreError::validation(format!("invalid escaped skill ID: {error}"))
                });
            }
        }
        Err(CoreError::validation("unterminated skill ID string"))
    }

    fn finish(&mut self) -> Result<()> {
        self.skip_whitespace();
        if self.cursor != self.bytes.len() {
            return Err(CoreError::validation(
                "unexpected data after skill JSON object",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_rank_tokens_and_preserves_insertion_position() {
        let json = SkillJsonDocument::parse(br#" { "alpha" : 1, "b\u0065ta":2 } "#).unwrap();
        assert_eq!(json.entries()["alpha"].value_range, 13..14);
        assert_eq!(json.entries()["beta"].rank, SkillRank::Elite);
        assert_eq!(json.insertion_offset(), 29);
    }

    #[test]
    fn duplicate_and_non_integer_members_fail_closed() {
        assert_eq!(
            SkillJsonDocument::parse(br#"{"alpha":1,"alpha":2}"#)
                .unwrap_err()
                .code,
            ErrorCode::AmbiguousStructure
        );
        assert!(SkillJsonDocument::parse(br#"{"alpha":1.0}"#).is_err());
        assert!(SkillJsonDocument::parse(br#"{"alpha":1,}"#).is_err());
    }
}
