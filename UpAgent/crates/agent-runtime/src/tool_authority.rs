//! Canonical authority classification for host-owned tools.
//!
//! The same classification drives dispatch, mutation/recovery decisions, and
//! plugin-execution provenance. Keeping one closed mapping prevents the durable
//! composition record from disagreeing with the actual privileged gateway.

/// Host authority that owns one built-in tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BuiltInToolAuthority {
    Effect,
    Memory,
    Interaction,
}

impl BuiltInToolAuthority {
    /// Stable identifier recorded in durable provenance.
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Effect => "upagent.kernel.effect-gateway",
            Self::Memory => "upagent.kernel.memory-gateway",
            Self::Interaction => "upagent.kernel.interaction-gateway",
        }
    }
}

/// Resolves the host authority for a built-in tool.
pub(crate) fn built_in_tool_authority(name: &str) -> Option<BuiltInToolAuthority> {
    if is_effect_tool(name) {
        Some(BuiltInToolAuthority::Effect)
    } else if is_memory_tool(name) {
        Some(BuiltInToolAuthority::Memory)
    } else if is_interaction_tool(name) {
        Some(BuiltInToolAuthority::Interaction)
    } else {
        None
    }
}

pub(crate) fn is_interaction_tool(name: &str) -> bool {
    matches!(name, "request_input")
}

pub(crate) fn is_effect_tool(name: &str) -> bool {
    matches!(
        name,
        "repo_map"
            | "search_code"
            | "read_file"
            | "write_file"
            | "replace_text"
            | "run_check"
            | "git_status"
            | "inspect_diff"
    )
}

pub(crate) fn is_mutating_tool(name: &str) -> bool {
    matches!(name, "write_file" | "replace_text" | "run_check")
}

pub(crate) fn is_source_mutating_tool(name: &str) -> bool {
    matches!(name, "write_file" | "replace_text")
}

pub(crate) fn is_memory_tool(name: &str) -> bool {
    matches!(name, "remember" | "forget_memory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_tool_has_one_authority() {
        for name in [
            "repo_map",
            "search_code",
            "read_file",
            "write_file",
            "replace_text",
            "run_check",
            "git_status",
            "inspect_diff",
            "remember",
            "forget_memory",
            "request_input",
        ] {
            assert!(built_in_tool_authority(name).is_some(), "{name}");
        }
        assert!(built_in_tool_authority("plugin_tool").is_none());
    }
}
