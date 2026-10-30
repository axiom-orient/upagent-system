use agent_core::{AgentFailure, FailureCode, ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;

const MAXIMUM_INPUT_OPTIONS: usize = 16;

#[derive(Debug, Clone)]
pub struct InputRequest {
    pub question: String,
    pub options: Vec<String>,
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "request_input".to_owned(),
        description: "Pause the current task and ask one concise user question only when the answer materially changes the result.".to_owned(),
        input_schema: json!({
            "type":"object",
            "properties":{
                "question":{"type":"string","minLength":1,"maxLength":8192},
                "options":{
                    "type":"array",
                    "items":{"type":"string","minLength":1,"maxLength":1024},
                    "maxItems":MAXIMUM_INPUT_OPTIONS
                }
            },
            "required":["question","options"],
            "additionalProperties":false
        }),
        strict: true,
    }
}

pub fn decode(call: &ToolCall) -> Result<InputRequest, AgentFailure> {
    let args: InputArgs = serde_json::from_value(call.arguments.clone()).map_err(|error| {
        AgentFailure::new(
            FailureCode::InvalidRequest,
            "request_input arguments are invalid",
        )
        .with_detail("cause", error.to_string())
    })?;
    let question = args.question.trim().to_owned();
    if question.is_empty() || question.len() > 8_192 {
        return Err(AgentFailure::new(
            FailureCode::InvalidRequest,
            "request_input question must be 1..=8192 bytes",
        ));
    }
    if args.options.len() > MAXIMUM_INPUT_OPTIONS {
        return Err(AgentFailure::new(
            FailureCode::InvalidRequest,
            format!("request_input may contain at most {MAXIMUM_INPUT_OPTIONS} options"),
        ));
    }
    let mut options = Vec::with_capacity(args.options.len());
    for option in args.options {
        let option = option.trim().to_owned();
        if option.is_empty() || option.len() > 1_024 || options.contains(&option) {
            return Err(AgentFailure::new(
                FailureCode::InvalidRequest,
                "request_input options must be unique non-empty values up to 1024 bytes",
            ));
        }
        options.push(option);
    }
    Ok(InputRequest { question, options })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InputArgs {
    question: String,
    #[serde(default)]
    options: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{decode, definition};
    use agent_core::{FailureCode, ToolCall, ToolCallId};

    #[test]
    fn input_options_accept_sixteen_and_reject_seventeen() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut call = ToolCall {
            id: ToolCallId::parse("input-options-bound")?,
            name: "request_input".to_owned(),
            arguments: serde_json::json!({
                "question": "Which option?",
                "options": (0..16).map(|index| format!("Option {index}")).collect::<Vec<_>>()
            }),
        };
        assert_eq!(decode(&call)?.options.len(), 16);
        call.arguments["options"]
            .as_array_mut()
            .ok_or("options must be an array")?
            .push(serde_json::json!("Option 16"));
        let failure = decode(&call).expect_err("seventeen options exceed the published limit");
        assert_eq!(failure.code, FailureCode::InvalidRequest);
        Ok(())
    }

    #[test]
    fn strict_schema_requires_options_even_when_runtime_defaults_it() {
        let schema = definition().input_schema;
        assert_eq!(
            schema["required"],
            serde_json::json!(["question", "options"])
        );
    }
}
