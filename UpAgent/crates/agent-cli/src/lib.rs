//! Provider-neutral composition API shared by the stock CLI and embedding hosts.
//!
//! Concrete provider connection and credentials remain host concerns. Callers
//! supply a linked-Rust `CompositionPluginCatalog`; the selected provider
//! binding and exact profile snapshot are then handed to `AgentRuntime`.

#![forbid(unsafe_code)]

pub mod composition;
