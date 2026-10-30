use rax_core::{ErrorCode, ErrorPhase, MAX_CONTROLLED_TEXT_BYTES, RaxError, TextEdit};

use crate::error::operation_error;

pub(super) fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Result<String, RaxError> {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| (edit.start, edit.end));
    let length = u64::try_from(source.len()).unwrap_or(u64::MAX);
    let mut previous_end = 0_u64;
    let mut capacity = source.len();
    for edit in &sorted {
        if edit.end > length || edit.start < previous_end {
            return Err(operation_error(
                ErrorCode::PreconditionFailed,
                ErrorPhase::Apply,
                "text edits are out of bounds or overlap",
            ));
        }
        let start = usize::try_from(edit.start).map_err(|_| {
            operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "edit offset is too large",
            )
        })?;
        let end = usize::try_from(edit.end).map_err(|_| {
            operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "edit offset is too large",
            )
        })?;
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(operation_error(
                ErrorCode::InvalidRequest,
                ErrorPhase::Apply,
                "text edit offsets must be UTF-8 character boundaries",
            ));
        }
        capacity = checked_output_capacity(
            capacity,
            end.checked_sub(start).ok_or_else(|| {
                operation_error(
                    ErrorCode::InvariantViolation,
                    ErrorPhase::Apply,
                    "validated edit range became invalid",
                )
            })?,
            edit.replacement.len(),
        )?;
        previous_end = edit.end;
    }

    let mut output = String::with_capacity(capacity);
    let mut cursor = 0_usize;
    for edit in sorted {
        let start = usize::try_from(edit.start).map_err(|_| {
            operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "edit offset is too large",
            )
        })?;
        let end = usize::try_from(edit.end).map_err(|_| {
            operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "edit offset is too large",
            )
        })?;
        output.push_str(&source[cursor..start]);
        output.push_str(&edit.replacement);
        cursor = end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

fn checked_output_capacity(
    current: usize,
    removed: usize,
    replacement: usize,
) -> Result<usize, RaxError> {
    let next = current
        .checked_sub(removed)
        .and_then(|value| value.checked_add(replacement))
        .ok_or_else(|| {
            operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "patched text size overflowed",
            )
        })?;
    if next > MAX_CONTROLLED_TEXT_BYTES {
        return Err(operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            "patched text exceeds the controlled-text byte limit",
        )
        .with_detail(
            "limit",
            u64::try_from(MAX_CONTROLLED_TEXT_BYTES).unwrap_or(u64::MAX),
        )
        .with_detail("size", u64::try_from(next).unwrap_or(u64::MAX)));
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_are_sorted_and_applied_once() {
        let edits = vec![
            TextEdit {
                start: 6,
                end: 11,
                replacement: "rax".to_string(),
            },
            TextEdit {
                start: 0,
                end: 5,
                replacement: "hello".to_string(),
            },
        ];
        assert_eq!(
            apply_text_edits("world terax", &edits).unwrap(),
            "hello rax"
        );
    }

    #[test]
    fn output_capacity_is_checked_without_allocating_the_limit() {
        assert!(checked_output_capacity(MAX_CONTROLLED_TEXT_BYTES, 0, 1).is_err());
        assert_eq!(
            checked_output_capacity(MAX_CONTROLLED_TEXT_BYTES, 1, 1).unwrap(),
            MAX_CONTROLLED_TEXT_BYTES
        );
    }

    #[test]
    fn overlapping_patches_are_rejected() {
        let edits = vec![
            TextEdit {
                start: 0,
                end: 2,
                replacement: String::new(),
            },
            TextEdit {
                start: 1,
                end: 3,
                replacement: String::new(),
            },
        ];
        assert!(apply_text_edits("abcd", &edits).is_err());
    }
}
