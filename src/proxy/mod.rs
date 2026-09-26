// Request records are the M1 foundation. Nothing in the binary calls them until ingress lands.
#[cfg_attr(not(test), allow(dead_code))]
mod types;

#[cfg_attr(not(test), allow(unused_imports))]
pub use types::{AttemptId, AttemptStatus, Request, RequestAttempt, RequestId};
