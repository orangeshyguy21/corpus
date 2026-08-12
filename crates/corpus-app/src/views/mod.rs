//! Screen modules (house rule): one module per screen; every
//! corpus-core call routes through `AppState`, widgets only render
//! state and request actions.

pub mod agents;
pub mod launch;
pub mod missions;
pub mod model_picker;
pub mod projects;