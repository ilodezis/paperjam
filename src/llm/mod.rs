//! The optional LLM layer: a chat-completions client (OpenRouter or any
//! OpenAI-compatible endpoint) with usage accounting and a monthly budget, and the pipeline steps built on it (transcription,
//! classification, fields).

pub mod budget;
pub mod client;
pub(crate) mod normalize;
pub mod schemas;
pub mod steps;
mod write;

pub use client::{Content, JsonRequest, LlmClient, Usage};
