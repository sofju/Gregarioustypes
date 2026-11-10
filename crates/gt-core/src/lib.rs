//! Shared plumbing: settings, paths, cancellation, encodings, formatting, logging.

pub mod cache;
pub mod cancel;
pub mod encoding;
pub mod fmt;
pub mod log;
pub mod paths;
pub mod settings;
pub mod sniff;

pub use cancel::CancelToken;
