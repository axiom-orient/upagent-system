use std::collections::BTreeMap;

use agent_contract::{identity::ToolCallId, model::ToolDefinition as ProviderTool};
use serde_json::Value;

use super::{
    MAX_TEXT_BYTES, MAX_TOOL_ARGUMENTS_BYTES, MAX_TOOL_CALL_ID_BYTES, MAX_TOOL_CALLS,
    NativeToolCall, ProviderRequest, ProviderResult, ProviderToolCall, failure, validate_tool_call,
};

pub(super) fn model_output_schema(request: &ProviderRequest<'_>) -> ProviderResult<Value> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "text".to_owned(),
        serde_json::json!({"type": "string", "maxLength": MAX_TEXT_BYTES}),
    );
    let mut required = vec![Value::String("text".to_owned())];
    if !request.tools.is_empty() {
        let mut tool_properties = serde_json::Map::new();
        let mut tool_names = Vec::with_capacity(request.tools.len());
        for tool in request.tools.iter() {
            original_schema_validator(&tool.input_schema)?;
            tool_names.push(Value::String(tool.name.clone()));
            tool_properties.insert(
                tool.name.clone(),
                serde_json::json!({
                    "type": "array",
                    "maxItems": MAX_TOOL_CALLS,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["id", "arguments"],
                        "properties": {
                            "id": {"type": "string", "maxLength": MAX_TOOL_CALL_ID_BYTES},
                            "arguments": tool.input_schema
                        }
                    }
                }),
            );
        }
        properties.insert(
            "toolCalls".to_owned(),
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": tool_names,
                "properties": tool_properties
            }),
        );
        required.push(Value::String("toolCalls".to_owned()));
    }
    let mut schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties
    });
    close_native_object_schemas(&mut schema)?;
    Ok(schema)
}

/// Native structured-output schemas require every object schema to explicitly
/// close additional properties and list every property as required. Tool
/// schemas come from the provider-neutral request, so normalize those rules
/// recursively before the schema crosses the native app-server boundary. An
/// explicitly open object schema is rejected rather than silently changing the
/// advertised tool contract; an omitted optional property becomes a required
/// nullable property, which is the native strict-schema representation.
/// Supported presence constraints use complete closed branches. Unsupported
/// array constraints are projected out of this decoding format and enforced
/// against the original schema before any call leaves this provider.
pub(super) fn close_native_object_schemas(schema: &mut Value) -> ProviderResult<()> {
    match schema {
        Value::Object(object) => {
            for keyword in [
                "allOf",
                "not",
                "dependentRequired",
                "dependentSchemas",
                "if",
                "then",
                "else",
            ] {
                if object.contains_key(keyword) {
                    return Err(failure(
                        "invalid_request",
                        &format!(
                            "native tool schema does not support {keyword} at this schema position"
                        ),
                        false,
                    ));
                }
            }
            if object.contains_key("properties") {
                reject_ambiguous_nullable_presence(object)?;
                lower_native_presence_branches(object)?;
            } else {
                lower_disjoint_native_union(object)?;
            }
            for key in [
                "contains",
                "minContains",
                "maxContains",
                "uniqueItems",
                "default",
            ] {
                object.remove(key);
            }
            normalize_native_const_type(object)?;
            let object_schema = match object.get("type") {
                Some(Value::String(kind)) => kind == "object",
                Some(Value::Array(types)) => {
                    types.iter().any(|value| value.as_str() == Some("object"))
                }
                _ => false,
            } || object.contains_key("properties")
                || object.contains_key("required");
            if object_schema {
                match object.get("additionalProperties") {
                    None | Some(Value::Bool(false)) => {}
                    Some(_) => {
                        return Err(failure(
                            "invalid_request",
                            "native strict object schemas require additionalProperties to be false",
                            false,
                        ));
                    }
                }
                object.insert("additionalProperties".to_owned(), Value::Bool(false));

                let property_names = object
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let existing_required = object
                    .get("required")
                    .map(|required| {
                        required
                            .as_array()
                            .ok_or_else(|| {
                                failure(
                                    "invalid_request",
                                    "native strict object schemas require required to be an array",
                                    false,
                                )
                            })?
                            .iter()
                            .map(|value| {
                                value.as_str().map(str::to_owned).ok_or_else(|| {
                                    failure(
                                        "invalid_request",
                                        "native strict object schema required entries must be strings",
                                        false,
                                    )
                                })
                            })
                            .collect::<ProviderResult<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                if existing_required
                    .iter()
                    .any(|name| !property_names.iter().any(|property| property == name))
                {
                    return Err(failure(
                        "invalid_request",
                        "native strict object schema required contains a property that is not declared",
                        false,
                    ));
                }
                if let Some(properties) =
                    object.get_mut("properties").and_then(Value::as_object_mut)
                {
                    for name in &property_names {
                        if !existing_required.iter().any(|required| required == name)
                            && let Some(property) = properties.get_mut(name)
                        {
                            make_native_property_nullable(property)?;
                        }
                    }
                }
                object.insert(
                    "required".to_owned(),
                    Value::Array(property_names.into_iter().map(Value::String).collect()),
                );
            }
            // Visit schema positions only: property names and annotation values
            // are data, even when they happen to be named `oneOf` or `type`.
            for key in ["properties", "$defs", "definitions", "patternProperties"] {
                if let Some(children) = object.get_mut(key).and_then(Value::as_object_mut) {
                    for child in children.values_mut() {
                        close_native_object_schemas(child)?;
                    }
                }
            }
            for key in [
                "items",
                "contains",
                "anyOf",
                "allOf",
                "prefixItems",
                "not",
                "if",
                "then",
                "else",
            ] {
                if let Some(child) = object.get_mut(key) {
                    close_native_object_schemas(child)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                close_native_object_schemas(item)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

/// `oneOf` is not in the native structured-output subset. A required,
/// distinct constant discriminator makes its branches mutually exclusive,
/// so `anyOf` has exactly the same meaning. Never weaken an arbitrary union.
fn lower_disjoint_native_union(object: &mut serde_json::Map<String, Value>) -> ProviderResult<()> {
    if !object.contains_key("oneOf") {
        return Ok(());
    }
    let branches = object
        .get("oneOf")
        .and_then(Value::as_array)
        .filter(|branches| !branches.is_empty());
    let disjoint = branches.is_some_and(|branches| {
        branches[0]
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| {
                properties.keys().any(|tag| {
                    let mut constants = Vec::new();
                    branches.iter().all(|branch| {
                        if branch.get("type").and_then(Value::as_str) != Some("object")
                            || !branch
                                .get("required")
                                .and_then(Value::as_array)
                                .is_some_and(|required| {
                                    required.iter().any(|key| key.as_str() == Some(tag))
                                })
                        {
                            return false;
                        }
                        let Some(value) = branch
                            .get("properties")
                            .and_then(|properties| properties.get(tag))
                            .and_then(|property| property.get("const"))
                            .filter(|value| value.is_string())
                        else {
                            return false;
                        };
                        if constants.contains(value) {
                            return false;
                        }
                        constants.push(value.clone());
                        true
                    })
                })
            })
    });
    if !disjoint || object.contains_key("anyOf") {
        return Err(failure(
            "invalid_request",
            "native oneOf requires disjoint object branches with a required unique string const discriminator",
            false,
        ));
    }
    if let Some(branches) = object.remove("oneOf") {
        object.insert("anyOf".to_owned(), branches);
    }
    Ok(())
}

fn required_names(value: Option<&Value>) -> Vec<String> {
    required_names_checked(value).unwrap_or_default()
}

fn required_names_checked(value: Option<&Value>) -> Option<Vec<String>> {
    let values = value?.as_array()?;
    let names = values
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (names.len()
        == names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len())
    .then_some(names)
}

fn reject_ambiguous_nullable_presence(
    object: &serde_json::Map<String, Value>,
) -> ProviderResult<()> {
    if !object.contains_key("oneOf") && !object.contains_key("anyOf") {
        return Ok(());
    }
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .expect("object properties");
    let required = required_names(object.get("required"));
    if properties
        .iter()
        .any(|(name, property)| !required.contains(name) && schema_allows_null(property))
    {
        return Err(failure(
            "invalid_request",
            "native schema cannot distinguish omission from an explicit nullable field in a presence constraint",
            false,
        ));
    }
    Ok(())
}

/// Supported presence unions use complete closed objects. Every branch keeps
/// every parent field, including unrelated required fields and optional controls.
fn lower_native_presence_branches(
    object: &mut serde_json::Map<String, Value>,
) -> ProviderResult<()> {
    if object.contains_key("oneOf") && object.contains_key("anyOf") {
        return Err(failure(
            "invalid_request",
            "native presence projection does not support simultaneous oneOf and anyOf",
            false,
        ));
    }
    let keyword = if object.contains_key("oneOf") {
        "oneOf"
    } else if object.contains_key("anyOf") {
        "anyOf"
    } else {
        return Ok(());
    };
    let invalid = || {
        failure(
            "invalid_request",
            "native presence union requires representable required/forbidden field branches",
            false,
        )
    };
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?
        .clone();
    let branches = object
        .get(keyword)
        .and_then(Value::as_array)
        .filter(|branches| !branches.is_empty())
        .ok_or_else(invalid)?;
    let names: Vec<_> = properties.keys().cloned().collect();
    // Already normalized complete branches must survive a second normalization.
    if keyword == "anyOf"
        && branches.iter().all(|branch| {
            branch.get("type").and_then(Value::as_str) == Some("object")
                && branch.get("additionalProperties") == Some(&Value::Bool(false))
                && branch
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|p| p.keys().cloned().collect::<Vec<_>>() == names)
                && required_names(branch.get("required"))
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    == names.iter().collect()
        })
    {
        return Ok(());
    }
    let parent_required = required_names(object.get("required"));
    let mut selections = Vec::new();
    let mut projected = Vec::new();
    for branch in branches {
        let branch = branch.as_object().ok_or_else(invalid)?;
        if branch
            .keys()
            .any(|key| !matches!(key.as_str(), "required" | "not" | "properties" | "type"))
            || branch
                .get("type")
                .is_some_and(|kind| kind.as_str() != Some("object"))
        {
            return Err(invalid());
        }
        let required = required_names_checked(branch.get("required"))
            .filter(|names| !names.is_empty())
            .ok_or_else(invalid)?;
        let forbidden = match branch.get("not") {
            Some(schema) => forbidden_presence_names(schema).ok_or_else(invalid)?,
            None if keyword == "anyOf" => Vec::new(),
            None => return Err(invalid()),
        };
        if required
            .iter()
            .chain(&forbidden)
            .any(|name| !properties.contains_key(name))
            || forbidden
                .iter()
                .any(|name| required.contains(name) || parent_required.contains(name))
        {
            return Err(invalid());
        }
        let mut branch_properties = properties.clone();
        if let Some(overrides) = branch.get("properties") {
            for (name, restriction) in overrides.as_object().ok_or_else(invalid)? {
                let property = branch_properties.get_mut(name).ok_or_else(invalid)?;
                let restrictions = restriction.as_object().ok_or_else(invalid)?;
                if forbidden.contains(name)
                    || restrictions
                        .keys()
                        .any(|key| !matches!(key.as_str(), "const" | "enum"))
                {
                    return Err(invalid());
                }
                let validator = original_schema_validator(property)?;
                if let Some(constant) = restrictions.get("const")
                    && !validator.is_valid(constant)
                {
                    return Err(invalid());
                }
                if let Some(values) = restrictions.get("enum")
                    && !values.as_array().is_some_and(|values| {
                        !values.is_empty() && values.iter().all(|value| validator.is_valid(value))
                    })
                {
                    return Err(invalid());
                }
                property
                    .as_object_mut()
                    .ok_or_else(invalid)?
                    .extend(restrictions.clone());
            }
        }
        for (name, property) in &mut branch_properties {
            if forbidden.contains(name) {
                *property = serde_json::json!({"type":"null"});
            } else if !required.contains(name) && !parent_required.contains(name) {
                make_native_property_nullable(property)?;
            }
        }
        selections.push((required, forbidden));
        projected.push(serde_json::json!({"type":"object", "additionalProperties":false, "required":names, "properties":branch_properties}));
    }
    if keyword == "oneOf" {
        if selections.len() != 2 {
            return Err(invalid());
        }
        let set = |values: &[String]| {
            values
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        if set(&selections[0].0) != set(&selections[1].1)
            || set(&selections[1].0) != set(&selections[0].1)
        {
            return Err(invalid());
        }
    }
    object.remove(keyword);
    object.insert("anyOf".to_owned(), Value::Array(projected));
    Ok(())
}

// `not: {required:[a,b]}` means not-both, not neither: only singleton
// requirements or an anyOf of singleton requirements express forbidden fields.
fn forbidden_presence_names(schema: &Value) -> Option<Vec<String>> {
    let object = schema.as_object()?;
    if object.len() != 1 {
        return None;
    }
    if object.contains_key("required") {
        return required_names_checked(object.get("required")).filter(|names| names.len() == 1);
    }
    let branches = object.get("anyOf")?.as_array()?;
    let mut result = Vec::new();
    for branch in branches {
        result.extend(forbidden_presence_names(branch)?);
    }
    (!result.is_empty()).then_some(result)
}

fn schema_allows_null(schema: &Value) -> bool {
    original_schema_validator(schema).is_ok_and(|validator| validator.is_valid(&Value::Null))
}

pub(super) fn original_schema_validator(schema: &Value) -> ProviderResult<jsonschema::Validator> {
    jsonschema::options()
        .offline()
        .with_pattern_options(jsonschema::PatternOptions::fancy_regex().backtrack_limit(100_000))
        .build(schema)
        .map_err(|_| {
            failure(
                "invalid_request",
                "tool inputSchema is invalid or requires an unavailable external reference",
                false,
            )
        })
}

fn native_json_type(value: &Value) -> Option<&'static str> {
    match value {
        Value::Null => Some("null"),
        Value::Bool(_) => Some("boolean"),
        Value::Number(number) if number_is_integral(number) => Some("integer"),
        Value::Number(_) => Some("number"),
        Value::String(_) => Some("string"),
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn number_is_integral(number: &serde_json::Number) -> bool {
    number.is_i64()
        || number.is_u64()
        || number
            .as_f64()
            .is_some_and(|value| value.is_finite() && value.fract() == 0.0)
}

fn normalize_native_const_type(object: &mut serde_json::Map<String, Value>) -> ProviderResult<()> {
    let Some(constant) = object.get("const").cloned() else {
        return Ok(());
    };
    let Some(kind) = native_json_type(&constant) else {
        return Err(failure(
            "invalid_request",
            "native strict schemas do not support array or object const values",
            false,
        ));
    };
    validate_const_enum_membership(object, &constant)?;
    if let Some(type_value) = object.get("type")
        && !native_type_includes(type_value, kind)
    {
        return Err(failure(
            "invalid_request",
            "native strict schema const value does not match its declared type",
            false,
        ));
    }
    object
        .entry("type".to_owned())
        .or_insert_with(|| Value::String(kind.to_owned()));
    Ok(())
}

fn validate_const_enum_membership(
    object: &serde_json::Map<String, Value>,
    constant: &Value,
) -> ProviderResult<()> {
    let Some(enum_value) = object.get("enum") else {
        return Ok(());
    };
    let values = enum_value.as_array().ok_or_else(|| {
        failure(
            "invalid_request",
            "native strict schema enum must be an array when const is present",
            false,
        )
    })?;
    if values
        .iter()
        .any(|value| primitive_schema_values_equal(value, constant))
    {
        Ok(())
    } else {
        Err(failure(
            "invalid_request",
            "native strict schema const value is excluded by enum",
            false,
        ))
    }
}

fn primitive_schema_values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null)
        | (Value::Bool(_), Value::Bool(_))
        | (Value::String(_), Value::String(_)) => left == right,
        (Value::Number(left), Value::Number(right)) => numeric_schema_values_equal(left, right),
        _ => false,
    }
}

fn numeric_schema_values_equal(left: &serde_json::Number, right: &serde_json::Number) -> bool {
    canonical_decimal(left) == canonical_decimal(right)
}

#[derive(Debug, Eq, PartialEq)]
struct CanonicalDecimal {
    negative: bool,
    digits: String,
    scale: i32,
}

fn canonical_decimal(number: &serde_json::Number) -> Option<CanonicalDecimal> {
    let text = number.to_string();
    let exponent_start = text.find(['e', 'E']);
    let (mantissa, exponent) = match exponent_start {
        Some(index) => (&text[..index], text[index + 1..].parse::<i32>().ok()?),
        None => (text.as_str(), 0),
    };
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa
        .strip_prefix('-')
        .or_else(|| mantissa.strip_prefix('+'))
        .unwrap_or(mantissa);
    let decimal_start = mantissa.find('.');
    let fractional_digits = match decimal_start {
        Some(index) => i32::try_from(mantissa.len() - index - 1).ok()?,
        None => 0,
    };
    let digits = mantissa.replace('.', "");
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let first_nonzero = digits.bytes().position(|byte| byte != b'0');
    let Some(first_nonzero) = first_nonzero else {
        return Some(CanonicalDecimal {
            negative: false,
            digits: "0".to_owned(),
            scale: 0,
        });
    };
    let mut digits = digits[first_nonzero..].to_owned();
    let mut scale = exponent.checked_sub(fractional_digits)?;
    while digits.ends_with('0') {
        digits.pop();
        scale = scale.checked_add(1)?;
    }
    Some(CanonicalDecimal {
        negative,
        digits,
        scale,
    })
}

fn native_type_includes(type_value: &Value, expected: &str) -> bool {
    match type_value {
        Value::String(kind) => native_type_pair_is_compatible(kind, expected),
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| native_type_pair_is_compatible(kind, expected)),
        _ => false,
    }
}

fn native_type_pair_is_compatible(declared: &str, actual: &str) -> bool {
    declared == actual || (declared == "number" && actual == "integer")
}

fn make_native_property_nullable(schema: &mut Value) -> ProviderResult<()> {
    if schema.as_object().is_some_and(|object| {
        ["anyOf", "oneOf", "allOf", "not", "$ref", "if"]
            .iter()
            .any(|key| object.contains_key(*key))
    }) {
        let original = schema.clone();
        *schema = serde_json::json!({"anyOf":[original, {"type":"null"}]});
        return Ok(());
    }
    if let Value::Object(object) = schema {
        if let Some(constant) = object.remove("const") {
            let Some(kind) = native_json_type(&constant) else {
                return Err(failure(
                    "invalid_request",
                    "native strict schemas do not support array or object const values",
                    false,
                ));
            };
            validate_const_enum_membership(object, &constant)?;
            if let Some(type_value) = object.get("type")
                && !native_type_includes(type_value, kind)
            {
                return Err(failure(
                    "invalid_request",
                    "native strict schema const value does not match its declared type",
                    false,
                ));
            }
            object
                .entry("type".to_owned())
                .or_insert_with(|| Value::String(kind.to_owned()));
            object.remove("enum");
            let mut enum_values = vec![constant];
            if kind != "null" {
                enum_values.push(Value::Null);
            }
            object.insert("enum".to_owned(), Value::Array(enum_values));
        }
        if let Some(type_value) = object.get("type")
            && (type_value.as_str() == Some("null")
                || type_value
                    .as_array()
                    .is_some_and(|types| types.iter().any(|value| value.as_str() == Some("null"))))
        {
            if let Some(enum_values) = object.get_mut("enum").and_then(Value::as_array_mut)
                && !enum_values.iter().any(Value::is_null)
            {
                enum_values.push(Value::Null);
            }
            return Ok(());
        }
        if object
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|variants| {
                variants.iter().any(|variant| {
                    variant
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind == "null")
                })
            })
        {
            return Ok(());
        }
        if let Some(type_value) = object.get_mut("type") {
            match type_value {
                Value::String(kind) => {
                    let kind = std::mem::take(kind);
                    *type_value =
                        Value::Array(vec![Value::String(kind), Value::String("null".to_owned())]);
                }
                Value::Array(types) => {
                    if !types.iter().any(|value| value.as_str() == Some("null")) {
                        types.push(Value::String("null".to_owned()));
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {}
            }
            if let Some(enum_values) = object.get_mut("enum").and_then(Value::as_array_mut)
                && !enum_values.iter().any(Value::is_null)
            {
                enum_values.push(Value::Null);
            }
            return Ok(());
        }
    }
    let original = schema.clone();
    *schema = serde_json::json!({
        "anyOf": [original, {"type": "null"}]
    });
    Ok(())
}

pub(super) fn flatten_native_tool_calls(
    mut grouped: BTreeMap<String, Vec<NativeToolCall>>,
    tools: &[ProviderTool],
) -> ProviderResult<Vec<ProviderToolCall>> {
    let mut flattened = Vec::new();
    for tool in tools {
        let calls = grouped.remove(&tool.name).ok_or_else(|| {
            failure(
                "invalid_provider_output",
                "native toolCalls omitted an advertised tool key",
                false,
            )
        })?;
        for call in calls {
            let mut arguments = call.arguments;
            restore_projection_nulls(&mut arguments, &tool.input_schema);
            flattened.push(ProviderToolCall {
                id: ToolCallId::parse(call.id)
                    .map_err(|error| failure("provider_failed", &error.to_string(), false))?,
                name: tool.name.clone(),
                arguments,
            });
            if flattened.len() > MAX_TOOL_CALLS {
                return Err(failure(
                    "resource_limit",
                    "aggregate native tool calls exceed the bounded provider limit",
                    false,
                ));
            }
        }
    }
    if !grouped.is_empty() {
        return Err(failure(
            "invalid_provider_output",
            "native toolCalls contained an unadvertised tool key",
            false,
        ));
    }
    Ok(flattened)
}

/// Remove only nulls injected by the native strict-schema projection. A null
/// declared by the original schema, or a field originally required, remains
/// visible to the UpAgent runtime for normal validation.
pub(super) fn restore_projection_nulls(value: &mut Value, schema: &Value) {
    match (&mut *value, schema) {
        (Value::Object(values), Value::Object(schema)) => {
            let required = required_names(schema.get("required"));
            let properties = schema.get("properties").and_then(Value::as_object);
            let names = values.keys().cloned().collect::<Vec<_>>();
            for name in names {
                let Some(property_schema) = properties.and_then(|properties| properties.get(&name))
                else {
                    continue;
                };
                let remove = values.get(&name).is_some_and(Value::is_null)
                    && !required.contains(&name)
                    && !schema_allows_null(property_schema);
                if remove {
                    values.remove(&name);
                } else if let Some(child) = values.get_mut(&name) {
                    restore_projection_nulls(child, property_schema);
                }
            }
        }
        (Value::Array(values), Value::Object(schema)) => {
            if let Some(item_schema) = schema.get("items") {
                for value in values {
                    restore_projection_nulls(value, item_schema);
                }
            }
        }
        _ => {}
    }
    // Restore inside union branches too. Only adopt an unambiguous restored
    // value that satisfies a branch; full root validation still follows.
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let mut candidates = Vec::new();
            for branch in branches {
                let mut candidate = value.clone();
                restore_projection_nulls(&mut candidate, branch);
                if original_schema_validator(branch)
                    .is_ok_and(|validator| validator.is_valid(&candidate))
                    && !candidates.contains(&candidate)
                {
                    candidates.push(candidate);
                }
            }
            if candidates.len() == 1 {
                *value = candidates.pop().expect("one restored candidate");
            }
        }
    }
}

pub(super) fn validate_returned_tools(
    tools: &[ProviderTool],
    calls: &[ProviderToolCall],
) -> ProviderResult<()> {
    if calls.len() > MAX_TOOL_CALLS {
        return Err(failure(
            "resource_limit",
            "provider returned too many tool calls",
            false,
        ));
    }
    let advertised = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut ids = std::collections::BTreeSet::new();
    let mut aggregate_argument_bytes = 0_usize;
    for call in calls {
        validate_tool_call(call)?;
        let argument_bytes = serde_json::to_vec(&call.arguments)
            .map_err(|error| failure("invalid_provider_output", &error.to_string(), false))?
            .len();
        aggregate_argument_bytes = aggregate_argument_bytes
            .checked_add(argument_bytes)
            .ok_or_else(|| {
                failure(
                    "resource_limit",
                    "aggregate tool-call argument size overflowed",
                    false,
                )
            })?;
        if aggregate_argument_bytes > MAX_TOOL_ARGUMENTS_BYTES {
            return Err(failure(
                "resource_limit",
                "aggregate tool-call arguments exceed the bounded byte limit",
                false,
            ));
        }
        if !advertised.contains(call.name.as_str()) {
            return Err(failure(
                "invalid_provider_output",
                "provider returned a tool outside the advertised set",
                false,
            ));
        }
        let tool = tools
            .iter()
            .find(|tool| tool.name == call.name)
            .expect("advertised tool");
        if let Err(error) = original_schema_validator(&tool.input_schema)?.validate(&call.arguments)
        {
            return Err(failure(
                "invalid_provider_output",
                &format!(
                    "tool {} arguments violate original inputSchema: instance={} schema={} keyword={}",
                    tool.name,
                    error
                        .instance_path()
                        .to_string()
                        .chars()
                        .take(256)
                        .collect::<String>(),
                    error
                        .schema_path()
                        .to_string()
                        .chars()
                        .take(256)
                        .collect::<String>(),
                    error.kind().keyword(),
                ),
                false,
            ));
        }
        if !ids.insert(&call.id) {
            return Err(failure(
                "invalid_provider_output",
                "provider returned duplicate tool call ids",
                false,
            ));
        }
    }
    Ok(())
}
