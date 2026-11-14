//! UI-framework-independent parts of the Volant desktop application.
//!
//! The download engine knows nothing about windows. This crate owns the shared
//! state model, controller, native messaging, clipboard and file-drop bridges.
//! GUI crates only translate user intent into [`control::Cmd`] and consume
//! [`control::UiEvent`].

pub mod capture;
pub mod clipboard;
pub mod control;
pub mod fmt;
pub mod log;
pub mod model;
pub mod native;
pub mod notify;
pub mod video;
