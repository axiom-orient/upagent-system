use agent_core::ToolCall;
use rax_core::{
    FsPatchInput, FsReadInput, MAX_CONTROLLED_TEXT_BYTES, MAX_EDIT_COUNT, Operation,
    OperationOutput, Outcome, RequestEnvelope, TextEdit,
};
use rax_runtime::Engine;
use serde::Deserialize;

use super::{EffectCancellationToken, EffectError, decode, default_read_bytes};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReplaceTextArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    #[serde(default = "default_read_bytes")]
    max_bytes: u64,
}

pub(super) fn replace_text_operation(
    engine: &Engine,
    call: &ToolCall,
    cancellation: &EffectCancellationToken,
) -> Result<Operation, EffectError> {
    let input: ReplaceTextArgs = decode(call)?;
    if input.old_string.is_empty() {
        return Err(EffectError::Contract(
            "replace_text oldString must not be empty".to_owned(),
        ));
    }
    if input.old_string.len() > MAX_CONTROLLED_TEXT_BYTES
        || input.new_string.len() > MAX_CONTROLLED_TEXT_BYTES
    {
        return Err(EffectError::Contract(format!(
            "replace_text strings must each be at most {MAX_CONTROLLED_TEXT_BYTES} bytes"
        )));
    }
    let read = engine.run_with_cancellation(
        RequestEnvelope {
            schema_version: rax_core::SCHEMA_VERSION.to_owned(),
            request_id: None,
            operation: Operation::FsRead(FsReadInput {
                path: input.path.clone(),
                max_bytes: input.max_bytes,
            }),
            record: false,
        },
        cancellation,
    )?;
    let Outcome::Succeeded {
        output: OperationOutput::FsRead(file),
        ..
    } = read.outcome
    else {
        return Err(EffectError::Contract(
            "replace_text could not obtain a complete text snapshot".to_owned(),
        ));
    };
    if file.truncated || file.encoding != "utf8" {
        return Err(EffectError::Contract(
            "replace_text requires a complete UTF-8 file snapshot".to_owned(),
        ));
    }
    let positions = file
        .content
        .match_indices(&input.old_string)
        .map(|(start, _)| start)
        .take(MAX_EDIT_COUNT.saturating_add(1))
        .collect::<Vec<_>>();
    if positions.is_empty() {
        return Err(EffectError::Contract(
            "replace_text oldString was not found".to_owned(),
        ));
    }
    if !input.replace_all && positions.len() != 1 {
        return Err(EffectError::Contract(format!(
            "replace_text requires exactly one match unless replaceAll is true; found {}",
            positions.len()
        )));
    }
    if positions.len() > MAX_EDIT_COUNT {
        return Err(EffectError::Contract(format!(
            "replace_text match count exceeds the bounded edit limit of {MAX_EDIT_COUNT}"
        )));
    }
    let selected = if input.replace_all {
        positions.as_slice()
    } else {
        &positions[..1]
    };
    let old_length = input.old_string.len();
    validate_replacement_budget(
        file.content.len(),
        old_length,
        input.new_string.len(),
        selected.len(),
    )?;
    let edits = selected
        .iter()
        .map(|start| {
            let start = u64::try_from(*start).map_err(|_| {
                EffectError::Contract("replace_text byte offset exceeds u64".to_owned())
            })?;
            let end_usize = usize::try_from(start)
                .map_err(|_| EffectError::Contract("replace_text offset is invalid".to_owned()))?
                .checked_add(old_length)
                .ok_or_else(|| EffectError::Contract("replace_text range overflow".to_owned()))?;
            let end = u64::try_from(end_usize).map_err(|_| {
                EffectError::Contract("replace_text byte offset exceeds u64".to_owned())
            })?;
            Ok(TextEdit {
                start,
                end,
                replacement: input.new_string.clone(),
            })
        })
        .collect::<Result<Vec<_>, EffectError>>()?;
    Ok(Operation::FsPatch(FsPatchInput {
        path: input.path,
        expected_sha256: file.sha256,
        max_bytes: input.max_bytes,
        edits,
    }))
}

pub(super) fn validate_replacement_budget(
    source_bytes: usize,
    old_bytes: usize,
    new_bytes: usize,
    replacement_count: usize,
) -> Result<(), EffectError> {
    let removed = old_bytes.checked_mul(replacement_count).ok_or_else(|| {
        EffectError::Contract("replace_text removed-byte count overflowed".to_owned())
    })?;
    let replacements = new_bytes.checked_mul(replacement_count).ok_or_else(|| {
        EffectError::Contract("replace_text replacement-byte count overflowed".to_owned())
    })?;
    if replacements > MAX_CONTROLLED_TEXT_BYTES {
        return Err(EffectError::Contract(format!(
            "replace_text replacement content exceeds {MAX_CONTROLLED_TEXT_BYTES} bytes"
        )));
    }
    let output = source_bytes
        .checked_sub(removed)
        .and_then(|value| value.checked_add(replacements))
        .ok_or_else(|| EffectError::Contract("replace_text output size overflowed".to_owned()))?;
    if output > MAX_CONTROLLED_TEXT_BYTES {
        return Err(EffectError::Contract(format!(
            "replace_text output exceeds {MAX_CONTROLLED_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}
