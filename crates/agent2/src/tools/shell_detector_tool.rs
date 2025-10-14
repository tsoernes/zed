use crate::{AgentTool, ToolCallEventStream};
use agent_client_protocol::ToolKind;
use anyhow::Result;
use gpui::{App, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Detects and lists available shells on the system.
///
/// This tool scans common shell locations and returns information about available shells,
/// including their paths and types. This is useful for determining which shell to use
/// when executing terminal commands.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShellDetectorToolInput {}

pub struct ShellDetectorTool;

impl ShellDetectorTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ShellInfo {
    name: String,
    path: String,
    exists: bool,
    shell_type: ShellType,
}

#[derive(Debug, Serialize, Deserialize)]
enum ShellType {
    Bash,
    Zsh,
    Fish,
    Sh,
    Dash,
    Ksh,
    Tcsh,
    Csh,
    Unknown,
}

impl AgentTool for ShellDetectorTool {
    type Input = ShellDetectorToolInput;
    type Output = String;

    fn name() -> &'static str {
        "detect_shells"
    }

    fn kind() -> ToolKind {
        ToolKind::Read
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> gpui::SharedString {
        "Detecting available shells...".into()
    }

    fn run(
        self: Arc<Self>,
        _input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let authorize = event_stream.authorize(self.initial_title(Ok(_input), cx), cx);
        cx.spawn(async move |_cx| {
            authorize.await?;

            let shells = detect_available_shells();
            format_shell_output(shells)
        })
    }
}

fn detect_available_shells() -> Vec<ShellInfo> {
    let common_shell_paths = vec![
        ("/bin/bash", "bash", ShellType::Bash),
        ("/usr/bin/bash", "bash", ShellType::Bash),
        ("/bin/zsh", "zsh", ShellType::Zsh),
        ("/usr/bin/zsh", "zsh", ShellType::Zsh),
        ("/usr/local/bin/zsh", "zsh", ShellType::Zsh),
        ("/bin/fish", "fish", ShellType::Fish),
        ("/usr/bin/fish", "fish", ShellType::Fish),
        ("/usr/local/bin/fish", "fish", ShellType::Fish),
        ("/bin/sh", "sh", ShellType::Sh),
        ("/usr/bin/sh", "sh", ShellType::Sh),
        ("/bin/dash", "dash", ShellType::Dash),
        ("/usr/bin/dash", "dash", ShellType::Dash),
        ("/bin/ksh", "ksh", ShellType::Ksh),
        ("/usr/bin/ksh", "ksh", ShellType::Ksh),
        ("/bin/tcsh", "tcsh", ShellType::Tcsh),
        ("/usr/bin/tcsh", "tcsh", ShellType::Tcsh),
        ("/bin/csh", "csh", ShellType::Csh),
        ("/usr/bin/csh", "csh", ShellType::Csh),
    ];

    let mut shells = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    for (path, name, shell_type) in common_shell_paths {
        let exists = Path::new(path).exists();

        if exists && !seen_names.contains(name) {
            seen_names.insert(name.to_string());
            shells.push(ShellInfo {
                name: name.to_string(),
                path: path.to_string(),
                exists,
                shell_type,
            });
        }
    }

    if let Ok(user_shell) = std::env::var("SHELL") {
        if !shells.iter().any(|s| s.path == user_shell) {
            let name = Path::new(&user_shell)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            shells.push(ShellInfo {
                name: name.clone(),
                path: user_shell,
                exists: true,
                shell_type: ShellType::Unknown,
            });
        }
    }

    shells
}

fn format_shell_output(shells: Vec<ShellInfo>) -> Result<String> {
    if shells.is_empty() {
        return Ok("No shells detected on the system.".to_string());
    }

    let mut output = String::from("Available shells:\n\n");

    for shell in shells {
        output.push_str(&format!("- **{}**: `{}`\n", shell.name, shell.path));
    }

    if let Ok(current_shell) = std::env::var("SHELL") {
        output.push_str(&format!(
            "\nCurrent shell (from $SHELL): `{}`\n",
            current_shell
        ));
    }

    output.push_str("\nYou can use any of these shells when executing terminal commands by specifying the full path or shell name.");

    Ok(output)
}
