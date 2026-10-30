use super::generation;
use super::path::{destination_error, managed_relative_path, reject_same_path};
use crate::{PpError, PpResult};

mod implementation;

pub(crate) use implementation::*;
