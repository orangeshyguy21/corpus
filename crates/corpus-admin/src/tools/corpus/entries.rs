//! Typed corpus-entry lifecycle tools.

mod delete;
mod r#move;
mod write;

pub(crate) use delete::DELETE;
pub(crate) use r#move::MOVE;
pub(crate) use write::WRITE;
