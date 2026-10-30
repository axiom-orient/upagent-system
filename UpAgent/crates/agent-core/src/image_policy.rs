//! Tool identity and classification. Definitions, activation and execution stay with
//! their existing owners; this is not a second runtime registry.

pub const IMAGE_GENERATE_TOOL: &str = "image_generate";
pub const IMAGE_INSPECT_TOOL: &str = "image_inspect";
pub const IMAGE_EXTRACT_TOOL: &str = "image_extract";
pub const IMAGE_RENDER_TOOL: &str = "image_render";
pub const IMAGE_COMPARE_TOOL: &str = "image_compare";
pub const IMAGE_EDIT_SEMANTIC_TOOL: &str = "image_edit_semantic";
pub const PROMPTGEN_COMPILE_TOOL: &str = "promptgen_compile";
pub const IMAGE_CAPABILITIES_TOOL: &str = "image_capabilities";
pub const IMAGEMAKER_PREPARE_TOOL: &str = "imagemaker_prepare";

pub const IMAGE_TOOL_NAMES: &[&str] = &[
    IMAGE_GENERATE_TOOL,
    IMAGE_INSPECT_TOOL,
    IMAGE_EXTRACT_TOOL,
    IMAGE_RENDER_TOOL,
    IMAGE_COMPARE_TOOL,
    IMAGE_EDIT_SEMANTIC_TOOL,
];
pub const IMAGE_MAKER_TOOL_NAMES: &[&str] = &[
    IMAGE_CAPABILITIES_TOOL,
    IMAGEMAKER_PREPARE_TOOL,
    PROMPTGEN_COMPILE_TOOL,
];

#[must_use]
pub fn is_image_tool_name(name: &str) -> bool {
    IMAGE_TOOL_NAMES.contains(&name)
}
#[must_use]
pub fn is_candidate_image_tool_name(name: &str) -> bool {
    matches!(
        name,
        IMAGE_GENERATE_TOOL | IMAGE_EXTRACT_TOOL | IMAGE_RENDER_TOOL | IMAGE_EDIT_SEMANTIC_TOOL
    )
}
#[must_use]
pub fn is_external_image_tool_name(name: &str) -> bool {
    matches!(name, IMAGE_GENERATE_TOOL | IMAGE_EDIT_SEMANTIC_TOOL)
}
#[must_use]
pub fn is_promptgen_tool_name(name: &str) -> bool {
    matches!(name, PROMPTGEN_COMPILE_TOOL | IMAGE_EDIT_SEMANTIC_TOOL)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classification_separates_compile_local_and_external_effects() {
        assert!(is_candidate_image_tool_name(IMAGE_EDIT_SEMANTIC_TOOL));
        assert!(is_external_image_tool_name(IMAGE_EDIT_SEMANTIC_TOOL));
        assert!(is_promptgen_tool_name(PROMPTGEN_COMPILE_TOOL));
        assert!(!is_external_image_tool_name(PROMPTGEN_COMPILE_TOOL));
        assert!(!is_candidate_image_tool_name(PROMPTGEN_COMPILE_TOOL));
        assert!(!is_image_tool_name("image_edit"));
        assert!(!is_external_image_tool_name(IMAGE_RENDER_TOOL));
    }
}
