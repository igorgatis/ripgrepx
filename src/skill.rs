//! `rgx --skill`: install the agent skill that teaches a model to prefer rgx, and print the MCP
//! registration instructions. The skill text is version-controlled in `assets/skill.md` and embedded
//! at build time so it can't drift from the binary (see `CLAUDE.md` — keep the skill in sync).

use std::path::PathBuf;

use anyhow::{Context, Result};

/// The skill document, embedded from the repo so the installed copy always matches this build.
const SKILL_MD: &str = include_str!("../assets/skill.md");

/// Install the skill under the user's Claude Code skills directory and print MCP setup hints.
/// Falls back to printing the skill to stdout if no home directory is available.
pub fn install() -> Result<()> {
    match skill_path() {
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("create {}", dir.display()))?;
            }
            std::fs::write(&path, SKILL_MD).with_context(|| format!("write {}", path.display()))?;
            println!("rgx: installed agent skill -> {}", path.display());
        }
        None => {
            println!("{SKILL_MD}");
        }
    }
    print_mcp_instructions();
    Ok(())
}

/// `$RGX_SKILL_DIR` override, else `~/.claude/skills/rgx/SKILL.md`.
fn skill_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RGX_SKILL_DIR") {
        return Some(PathBuf::from(dir).join("rgx").join("SKILL.md"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude/skills/rgx/SKILL.md"))
}

fn print_mcp_instructions() {
    println!(
        "\nTo expose rgx to agents over MCP, register it as a stdio MCP server:\n\
         \n  claude mcp add rgx -- rgx --server mcp\n\
         \nor add to your MCP client config:\n\
         \n  \"rgx\": {{ \"command\": \"rgx\", \"args\": [\"--server\", \"mcp\"] }}\n\
         \nThe MCP server exposes content_search, file_search, and status tools."
    );
}
