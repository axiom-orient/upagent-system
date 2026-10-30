use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rax_core::{ErrorCode, ErrorPhase, RaxError};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) fn now_rfc3339() -> Result<String, RaxError> {
    format_rfc3339(OffsetDateTime::now_utc(), ErrorPhase::Record)
}

pub(crate) fn rfc3339_after(duration: Duration) -> Result<String, RaxError> {
    let seconds = i64::try_from(duration.as_secs()).map_err(|_| {
        RaxError::new(
            ErrorCode::LimitExceeded,
            ErrorPhase::Plan,
            false,
            "duration exceeds the supported timestamp range",
        )
    })?;
    let target = OffsetDateTime::now_utc()
        .checked_add(time::Duration::seconds(seconds))
        .ok_or_else(|| {
            RaxError::new(
                ErrorCode::LimitExceeded,
                ErrorPhase::Plan,
                false,
                "duration exceeds the supported timestamp range",
            )
        })?;
    format_rfc3339(target, ErrorPhase::Plan)
}

pub(crate) fn parse_rfc3339(value: &str, phase: ErrorPhase) -> Result<OffsetDateTime, RaxError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|error| {
        RaxError::new(
            ErrorCode::ProtocolError,
            phase,
            false,
            format!("invalid RFC 3339 timestamp: {error}"),
        )
        .with_detail("timestamp", value.to_string())
    })
}

pub(crate) fn unix_millis(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn format_rfc3339(value: OffsetDateTime, phase: ErrorPhase) -> Result<String, RaxError> {
    value.format(&Rfc3339).map_err(|error| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            phase,
            false,
            format!("failed to format RFC 3339 timestamp: {error}"),
        )
    })
}
