//! Exit codes per §22.9.

pub const SUCCESS: i32 = 0;
pub const GENERAL_ERROR: i32 = 1;
pub const TARGET_NOT_FOUND: i32 = 2;
pub const AMBIGUOUS_TARGET: i32 = 3;
pub const HOST_START_FAILED: i32 = 4;
pub const DEFINITION_ERROR: i32 = 5;
pub const CONNECTION_ERROR: i32 = 6;
pub const TIMEOUT: i32 = 10;
