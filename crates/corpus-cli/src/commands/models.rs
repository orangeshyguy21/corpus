//! Model-registry CLI commands.

use corpus_core::ModelRegistry;

use crate::cli::ModelsCommand;

pub(crate) fn run(command: ModelsCommand) -> Result<(), String> {
    match command {
        ModelsCommand::List => list(),
    }
}

fn list() -> Result<(), String> {
    let registry = ModelRegistry::load_default().map_err(|error| error.to_string())?;
    for model in &registry.models {
        println!(
            "{:<20} {:<8} {:<10} {}",
            model.tag,
            model
                .params_b
                .map(|params| format!("{params}B"))
                .unwrap_or_else(|| "-".to_string()),
            model.provider,
            model.capabilities.join(",")
        );
    }
    if registry.models.is_empty() {
        println!("no models registered");
    }
    Ok(())
}
