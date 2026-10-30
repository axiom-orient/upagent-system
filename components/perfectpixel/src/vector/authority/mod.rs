//! Immutable, embedded authority for internal vector publication routes.
//!
//! The registry has no filesystem, environment, or caller-supplied authority source. Analysis may
//! inspect this authority but receives no publication capability from it.

mod routes;
mod thresholds;

pub(crate) use routes::{RouteEntry, RouteKey, RouteRegistry, RouteState};
pub(crate) use thresholds::{FixedGates, ThresholdBundle, ThresholdDefaults};

use super::candidate::sha256_hex;
use std::fmt;

/// Embedded-authority digest validators, shared by the route and threshold registries.
///
/// These are deliberately stricter than a plain hex check: an all-zero digest is the
/// canonical "unset placeholder" value, so accepting one would let an uninitialized record
/// pass as verified authority. Code outside `authority` that only needs a syntactic hex check
/// must not reuse these.
fn is_nonzero_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value.bytes().any(|byte| byte != b'0')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_sha256_identity(value: &str) -> bool {
    value
        .strip_prefix("sha256-")
        .is_some_and(is_nonzero_sha256_hex)
}

fn is_sha256_hex(value: &str) -> bool {
    is_nonzero_sha256_hex(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthorityError {
    ThresholdRegistry(String),
    RouteRegistry(String),
    #[cfg(test)]
    Transition(String),
}

impl fmt::Display for AuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThresholdRegistry(message) => {
                write!(formatter, "invalid embedded threshold registry: {message}")
            }
            Self::RouteRegistry(message) => {
                write!(formatter, "invalid embedded route registry: {message}")
            }
            #[cfg(test)]
            Self::Transition(message) => {
                write!(formatter, "invalid route transition: {message}")
            }
        }
    }
}

impl std::error::Error for AuthorityError {}

pub(crate) type AuthorityResult<T> = Result<T, AuthorityError>;

#[derive(Debug, Clone)]
pub(crate) struct VerifiedAuthority {
    thresholds: ThresholdBundle,
    routes: RouteRegistry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityInvariants {
    pub(crate) threshold_set_id: String,
    pub(crate) threshold_bundle_digest: String,
    pub(crate) foundation_id: String,
    pub(crate) route_registry_id: String,
    pub(crate) route_registry_digest: String,
    pub(crate) fixed_gates: FixedGates,
}

impl VerifiedAuthority {
    /// Constructs the only authority source: verified registries compiled into this crate.
    pub(crate) fn embedded() -> AuthorityResult<Self> {
        let thresholds = ThresholdBundle::embedded()?;
        let routes = RouteRegistry::embedded(&thresholds)?;
        Ok(Self { thresholds, routes })
    }

    pub(crate) fn route(&self, key: &RouteKey) -> Option<&RouteEntry> {
        self.routes.route(key)
    }
    pub(crate) fn thresholds(&self) -> &ThresholdBundle {
        &self.thresholds
    }

    pub(crate) fn invariants(&self) -> AuthorityInvariants {
        AuthorityInvariants {
            threshold_set_id: self.thresholds.id().to_owned(),
            threshold_bundle_digest: self.thresholds.digest().to_owned(),
            foundation_id: self.thresholds.foundation_id().to_owned(),
            route_registry_id: self.routes.id().to_owned(),
            route_registry_digest: self.routes.digest().to_owned(),
            fixed_gates: self.thresholds.fixed_gates().clone(),
        }
    }
}

fn digest(value: &str) -> String {
    format!("sha256-{}", sha256_hex(value.as_bytes()))
}
