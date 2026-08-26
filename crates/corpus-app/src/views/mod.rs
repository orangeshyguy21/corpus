//! Screen modules (house rule): one module per screen; every
//! corpus-core call routes through `AppState`, widgets only render
//! state and request actions.

pub mod agents;
pub mod components;
mod finding_summary;
pub mod mission_actions;
pub mod missions;
pub mod model_picker;
pub mod plugin_picker;
pub mod policy;
pub mod projects;
pub mod source_dropdown;
pub mod syntax_editor;
