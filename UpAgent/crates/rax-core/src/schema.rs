use schemars::{schema::RootSchema, schema_for};

use crate::{Operation, RequestEnvelope, ResultEnvelope};

#[must_use]
pub fn request_schema() -> RootSchema {
    schema_for!(RequestEnvelope)
}

#[must_use]
pub fn result_schema() -> RootSchema {
    schema_for!(ResultEnvelope)
}

#[must_use]
pub fn operation_schema() -> RootSchema {
    schema_for!(Operation)
}
