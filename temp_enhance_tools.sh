#!/usr/bin/env bash
set -euo pipefail
FILE="crates/agent2/src/thread.rs"
backup="${FILE}.pre_enhanced.bak"
cp "$FILE" "$backup"

# Inject EnhancedTerminalTool into import block if missing
perl -0777 -pi -e 's/(EditFileTool, )(FetchTool)/$1EnhancedTerminalTool, $2/ unless /EnhancedTerminalTool/' "$FILE"
# Inject ShellDetectorTool into import block if missing
perl -0777 -pi -e 's/(OpenTool, ReadFileTool, )(SystemPromptTemplate)/$1ShellDetectorTool, $2/ unless /ShellDetectorTool/' "$FILE"

# Modify add_default_tools registrations
perl -0777 -pi -e 's/self.add_tool\(TerminalTool::new\(self.project.clone\(\), environment\)\);/self.add_tool(TerminalTool::new(self.project.clone(), environment.clone()));\n        self.add_tool(EnhancedTerminalTool::new(self.project.clone(), environment.clone()));\n        self.add_tool(ShellDetectorTool::new());/ unless /EnhancedTerminalTool::new/' "$FILE"

# Ensure we cloned environment for later lines (idempotent)
perl -0777 -pi -e 's/TerminalTool::new\(self.project.clone\(\), environment\)/TerminalTool::new(self.project.clone(), environment.clone())/g' "$FILE"

# Show summary of changes
echo '--- Added tool imports? ---'
grep -n 'EnhancedTerminalTool' "$FILE" || true
grep -n 'ShellDetectorTool' "$FILE" || true
echo '--- Tool registration lines (context) ---'
grep -n 'EnhancedTerminalTool::new' "$FILE" || true
grep -n 'ShellDetectorTool::new' "$FILE" || true

echo 'Backup stored at: ' "$backup"
