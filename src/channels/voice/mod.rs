//! Voice Processing Module
//!
//! Speech-to-text (Groq Whisper) and text-to-speech (OpenAI TTS) services.

mod service;

pub use service::{synthesize_speech, transcribe_audio};
