pub(crate) mod anthropic_messages;
#[cfg_attr(not(test), allow(dead_code))]
mod anthropic_upstream;
pub(crate) mod openai_chat;
pub(crate) mod openai_responses;
#[cfg_attr(not(test), allow(dead_code))]
mod openai_upstream;
mod types;

#[cfg_attr(not(test), allow(unused_imports))]
pub use anthropic_upstream::{AnthropicError, AnthropicEventStream, AnthropicUpstream};
#[cfg_attr(not(test), allow(unused_imports))]
pub use openai_upstream::{OpenAiUpstream, SteveError};
pub use types::{AttemptId, AttemptStatus, Request, RequestAttempt, RequestId};

// Stream pump and cancel helpers. Ingress routes call these once they land.
#[cfg_attr(not(test), allow(dead_code))]
pub mod stream;
