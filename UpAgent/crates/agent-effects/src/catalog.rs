use agent_core::ToolDefinition;
use serde_json::{Value, json};

/// Pure semantic tool definitions independent of a workspace or RAX state.
#[must_use]
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool(
            "repo_map",
            "List a bounded repository subtree in deterministic order. The path is workspace-relative; use an empty string for the workspace root and never pass an absolute path.",
            json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "maxDepth":{"type":"integer","minimum":1,"maximum":64},
                    "maxEntries":{"type":"integer","minimum":1,"maximum":100_000}
                },
                "required":["path","maxDepth","maxEntries"],
                "additionalProperties":false
            }),
        ),
        tool(
            "search_code",
            "Search bounded workspace text using a regular expression or literal pattern. The path is workspace-relative; use an empty string for the workspace root and never pass an absolute path.",
            json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "pattern":{"type":"string","minLength":1},
                    "regex":{"type":"boolean"},
                    "caseSensitive":{"type":"boolean"},
                    "maxMatches":{"type":"integer","minimum":1,"maximum":100_000}
                },
                "required":["path","pattern","regex","caseSensitive","maxMatches"],
                "additionalProperties":false
            }),
        ),
        tool(
            "read_file",
            "Read one workspace-relative file and return content with SHA-256. Never pass the absolute workspace path.",
            json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string","minLength":1},
                    "maxBytes":{"type":"integer","minimum":1,"maximum":67_108_864}
                },
                "required":["path","maxBytes"],
                "additionalProperties":false
            }),
        ),
        tool(
            "write_file",
            "Create or replace one workspace-relative file. Replacing requires the expected SHA-256; never pass the absolute workspace path.",
            json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string","minLength":1},
                    "content":{"type":"string"},
                    "createOnly":{"type":"boolean"},
                    "expectedSha256":{"type":["string","null"]}
                },
                "required":["path","content","createOnly","expectedSha256"],
                "additionalProperties":false
            }),
        ),
        tool(
            "replace_text",
            "Replace one exact text occurrence, or all exact occurrences, in a workspace-relative file using an immutable file precondition. Never pass the absolute workspace path.",
            json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string","minLength":1},
                    "oldString":{"type":"string","minLength":1},
                    "newString":{"type":"string"},
                    "replaceAll":{"type":"boolean"},
                    "maxBytes":{"type":"integer","minimum":1,"maximum":67_108_864}
                },
                "required":["path","oldString","newString","replaceAll","maxBytes"],
                "additionalProperties":false
            }),
        ),
        tool(
            "run_check",
            "Run one argv-based verification command in the workspace. When the command proves an AXD required check, set the optional requirementId to that exact identifier. Shell command strings are not accepted; cwd must be workspace-relative or null for the workspace root.",
            json!({
                "type":"object",
                "properties":{
                    "requirementId":{"type":["string","null"],"minLength":1,"maxLength":256},
                    "program":{"type":"string","minLength":1},
                    "args":{"type":"array","items":{"type":"string"},"maxItems":4096},
                    "cwd":{"type":["string","null"]},
                    "timeoutMs":{"type":"integer","minimum":1,"maximum":3_600_000},
                    "maxOutputBytes":{"type":"integer","minimum":1,"maximum":67_108_864}
                },
                "required":["requirementId","program","args","cwd","timeoutMs","maxOutputBytes"],
                "additionalProperties":false
            }),
        ),
        tool(
            "git_status",
            "Read typed Git repository status. The repository path is workspace-relative; use an empty string for the workspace root and never pass an absolute path.",
            json!({
                "type":"object",
                "properties":{"repository":{"type":"string"}},
                "required":["repository"],
                "additionalProperties":false
            }),
        ),
        tool(
            "inspect_diff",
            "Read a bounded Git diff. At preflight observation, scoped untracked or ignored entries fail closed; this is not an atomic snapshot against external concurrent mutation. Git diff excludes untracked files; empty output is not evidence that a new file exists or has specific content. For a newly created file, use read_file to verify exact content and git_status to verify untracked presence. Repository and file paths are workspace-relative; use an empty repository path for the workspace root and never pass an absolute path.",
            json!({
                "type":"object",
                "properties":{
                    "repository":{"type":"string"},
                    "staged":{"type":"boolean"},
                    "paths":{"type":"array","items":{"type":"string"}},
                    "maxBytes":{"type":"integer","minimum":1,"maximum":67_108_864}
                },
                "required":["repository","staged","paths","maxBytes"],
                "additionalProperties":false
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        strict: true,
    }
}

#[cfg(test)]
mod tests {
    use super::tool_definitions;

    #[test]
    fn inspect_diff_contract_warns_about_untracked_files() {
        let tool = tool_definitions()
            .into_iter()
            .find(|tool| tool.name == "inspect_diff")
            .expect("inspect_diff tool definition");
        assert!(
            tool.description
                .contains("Git diff excludes untracked files")
        );
        assert!(
            tool.description.contains(
                "At preflight observation, scoped untracked or ignored entries fail closed"
            )
        );
        assert!(
            tool.description
                .contains("not an atomic snapshot against external concurrent mutation")
        );
        assert!(
            tool.description
                .contains("empty output is not evidence that a new file exists")
        );
        assert!(tool.description.contains(
            "use read_file to verify exact content and git_status to verify untracked presence"
        ));
    }
}
