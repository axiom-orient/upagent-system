use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

pub use agent_contract::identity::{ArtifactId, IdentifierError, ToolCallId};

macro_rules! identifier {
    ($name:ident, $label:literal, $maximum:literal, $allow_slash:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                let valid = !value.is_empty()
                    && value.len() <= $maximum
                    && value.chars().all(|character| {
                        character.is_ascii_alphanumeric()
                            || matches!(character, '-' | '_' | '.' | ':')
                            || ($allow_slash && character == '/')
                    });
                if !valid {
                    return Err(IdentifierError::Invalid {
                        label: $label,
                        maximum: $maximum,
                    });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

identifier!(TaskId, "task_id", 160, false);
identifier!(SessionId, "session_id", 160, false);
identifier!(ProfileId, "profile_id", 96, false);
identifier!(TrackId, "track_id", 96, false);
identifier!(SkillId, "skill_id", 160, true);
identifier!(CapabilityId, "capability_id", 160, true);
identifier!(ReceiptId, "receipt_id", 160, false);
impl ProfileId {
    #[must_use]
    pub fn creator() -> Self {
        Self("creator".to_owned())
    }

    #[must_use]
    pub fn developer() -> Self {
        Self("developer".to_owned())
    }

    #[must_use]
    pub fn twin() -> Self {
        Self("twin".to_owned())
    }

    #[must_use]
    pub fn reviewer() -> Self {
        Self("reviewer".to_owned())
    }
}

impl TrackId {
    #[must_use]
    pub fn prototyper() -> Self {
        Self("prototyper".to_owned())
    }

    #[must_use]
    pub fn builder() -> Self {
        Self("builder".to_owned())
    }

    #[must_use]
    pub fn sweeper() -> Self {
        Self("sweeper".to_owned())
    }

    #[must_use]
    pub fn grower() -> Self {
        Self("grower".to_owned())
    }

    #[must_use]
    pub fn maintainer() -> Self {
        Self("maintainer".to_owned())
    }

    #[must_use]
    pub fn review_neutral() -> Self {
        Self("review-neutral".to_owned())
    }

    #[must_use]
    pub fn review_critical() -> Self {
        Self("review-critical".to_owned())
    }

    #[must_use]
    pub fn review_adversarial() -> Self {
        Self("review-adversarial".to_owned())
    }

    #[must_use]
    pub fn review_fact_checker() -> Self {
        Self("review-fact-checker".to_owned())
    }
}
