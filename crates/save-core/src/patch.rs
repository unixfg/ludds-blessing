use crate::error::{CoreError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// A lossless mutation guarded by the bytes observed when the review was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanPatch {
    pub start: u64,
    pub end: u64,
    #[serde(skip)]
    pub expected: Vec<u8>,
    #[serde(skip)]
    pub replacement: Vec<u8>,
    pub label: String,
}

impl SpanPatch {
    pub fn new(
        range: Range<usize>,
        expected: impl Into<Vec<u8>>,
        replacement: impl Into<Vec<u8>>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            start: range.start as u64,
            end: range.end as u64,
            expected: expected.into(),
            replacement: replacement.into(),
            label: label.into(),
        }
    }

    pub fn range(&self) -> Result<Range<usize>> {
        let start = usize::try_from(self.start)
            .map_err(|_| CoreError::validation("patch start is too large for this platform"))?;
        let end = usize::try_from(self.end)
            .map_err(|_| CoreError::validation("patch end is too large for this platform"))?;
        Ok(start..end)
    }
}

/// Applies non-overlapping patches without reserializing any unedited bytes.
pub fn apply_patches(source: &[u8], patches: &[SpanPatch]) -> Result<Vec<u8>> {
    let mut ordered: Vec<&SpanPatch> = patches.iter().collect();
    ordered.sort_by_key(|patch| (patch.start, patch.end));

    let mut previous_end = 0usize;
    let mut insertion_at_previous_end = false;
    let mut output_capacity = source.len();

    for patch in &ordered {
        let range = patch.range()?;
        if range.start > range.end || range.end > source.len() {
            return Err(CoreError::validation(format!(
                "patch '{}' is outside the source document",
                patch.label
            )));
        }
        let is_insertion = range.start == range.end;
        if range.start < previous_end
            || (is_insertion && insertion_at_previous_end && range.start == previous_end)
        {
            return Err(CoreError::validation(format!(
                "patch '{}' overlaps another patch",
                patch.label
            )));
        }
        if source[range.clone()] != patch.expected {
            return Err(CoreError::new(
                ErrorCode::StaleSave,
                format!("source bytes changed at patch '{}'", patch.label),
            ));
        }
        output_capacity = output_capacity
            .saturating_sub(range.len())
            .saturating_add(patch.replacement.len());
        previous_end = range.end;
        insertion_at_previous_end = is_insertion;
    }

    let mut output = Vec::with_capacity(output_capacity);
    let mut cursor = 0usize;
    for patch in ordered {
        let range = patch.range()?;
        output.extend_from_slice(&source[cursor..range.start]);
        output.extend_from_slice(&patch.replacement);
        cursor = range.end;
    }
    output.extend_from_slice(&source[cursor..]);
    Ok(output)
}

#[derive(Debug, Default)]
pub(crate) struct PatchBuilder {
    patches: Vec<SpanPatch>,
}

impl PatchBuilder {
    pub fn push(&mut self, patch: SpanPatch) -> Result<()> {
        let range = patch.range()?;
        if let Some(existing) = self
            .patches
            .iter_mut()
            .find(|existing| existing.start == patch.start && existing.end == patch.end)
        {
            if existing.expected != patch.expected {
                return Err(CoreError::validation("conflicting expected patch bytes"));
            }
            // Multiple semantic updates can converge on the same source field.
            existing.replacement = patch.replacement;
            existing.label = patch.label;
            return Ok(());
        }
        for existing in &self.patches {
            let other = existing.range()?;
            let overlaps = range.start < other.end && other.start < range.end;
            if overlaps {
                return Err(CoreError::validation(format!(
                    "patch '{}' overlaps '{}'",
                    patch.label, existing.label
                )));
            }
        }
        self.patches.push(patch);
        Ok(())
    }

    pub fn finish(mut self) -> Vec<SpanPatch> {
        self.patches.sort_by_key(|patch| (patch.start, patch.end));
        self.patches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_patches_preserve_all_other_bytes() {
        let source = b"alpha beta gamma";
        assert_eq!(apply_patches(source, &[]).unwrap(), source);
        let patches = vec![SpanPatch::new(
            6..10,
            b"beta".to_vec(),
            b"B".to_vec(),
            "word",
        )];
        assert_eq!(apply_patches(source, &patches).unwrap(), b"alpha B gamma");
    }

    #[test]
    fn stale_and_overlapping_patches_fail_closed() {
        let source = b"abcdef";
        let stale = SpanPatch::new(1..2, b"x".to_vec(), b"B".to_vec(), "stale");
        assert_eq!(
            apply_patches(source, &[stale]).unwrap_err().code,
            ErrorCode::StaleSave
        );

        let overlap = vec![
            SpanPatch::new(1..4, b"bcd".to_vec(), b"x".to_vec(), "one"),
            SpanPatch::new(3..5, b"de".to_vec(), b"y".to_vec(), "two"),
        ];
        assert_eq!(
            apply_patches(source, &overlap).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
    }
}
