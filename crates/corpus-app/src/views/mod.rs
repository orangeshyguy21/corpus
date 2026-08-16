//! Screen modules (house rule): one module per screen; every
//! corpus-core call routes through `AppState`, widgets only render
//! state and request actions.

pub mod agents;
pub mod json_editor;
pub mod missions;
pub mod plugin_picker;
pub mod projects;
pub mod source_dropdown;