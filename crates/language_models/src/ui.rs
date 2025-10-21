pub mod instruction_list_item;
pub use instruction_list_item::InstructionListItem;

pub mod azure_foundry_config {
    /// Placeholder configuration panel entry for Azure Foundry.
    ///
    /// Acts as a no-op holder so other UI components can reference this symbol
    /// when wiring provider settings without requiring additional files.
    pub struct AzureFoundryConfigPanel;
}

pub use azure_foundry_config::AzureFoundryConfigPanel;
