//! Typed agent administration tools.

mod common;
mod delete;
mod mutations;
mod persistence;
mod read;

pub(crate) use delete::DELETE;
pub(crate) use mutations::{SET, SET_PERMISSION, SET_ROLE, SUBAGENT_ADD, SUBAGENT_REMOVE};
pub(crate) use persistence::{CLONE, COPY, NEW, SAVE};
pub(crate) use read::{GET, LIST};
