//! Reusable application coordination surface shared by the GUI and tests.

#![allow(clippy::items_after_test_module)]

pub mod chat;
pub mod diagnostics;
mod file_watch;
pub mod fmt;
pub mod jobs;
pub mod nav;
mod observability;
mod session_service;
pub mod sidebar;
pub mod state;
pub mod terminal;
pub mod theme;
pub mod views;
