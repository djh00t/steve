pub(crate) mod anthropic_messages;
pub(crate) mod openai_chat;
#[cfg_attr(not(test), allow(dead_code))]
mod openai_upstream;
mod types;

#[cfg_attr(not(test), allow(unused_imports))]
pub use openai_upstream::{OpenAiUpstream, SteveError};
pub use types::{AttemptId, AttemptStatus, Request, RequestAttempt, RequestId};

// Stream pump and cancel helpers. Ingress routes call these once they land.
#[cfg_attr(not(test), allow(dead_code))]
pub mod stream;
