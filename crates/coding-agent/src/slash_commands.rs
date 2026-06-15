//! Slash command handlers for the TUI.
//!
//! Each handler receives the raw command text, a reference to the config,
//! and appends results to the transcript via the [`TranscriptHandle`] trait
//! (`push_system` / `push_error` / `clear`). The concrete implementor is the
//! reactive `UiState` (wrapped in `Rc`), so the command bodies stay unchanged
//! from the old `&mut Transcript` API.

use crate::config::Config;
use crate::tui::state::TranscriptHandle;

/// Dispatch a slash command. Returns `true` if the user wants to quit.
pub fn handle_slash_command(
    text: &str,
    transcript: &mut dyn TranscriptHandle,
    config: &Config,
) -> bool {
    let parts: Vec<&str> = text.splitn(3, ' ').collect();
    let cmd = parts[0];

    match cmd {
        "/help" | "/h" | "/?" => handle_help(transcript),
        "/clear" | "/cls" => {
            transcript.clear();
        }
        "/quit" | "/exit" | "/q" => return true,
        "/mcp" => handle_mcp(parts.get(1).copied().unwrap_or(""), config, transcript),
        "/skills" => handle_skills(config, transcript),
        "/model" | "/compact" => {
            // Placeholder: these will be implemented later
            transcript.push_error(format!("{} is not yet implemented. Coming soon!", cmd));
        }
        _ => {
            transcript.push_error(format!(
                "Unknown command: {cmd}. Type /help for available commands."
            ));
        }
    }
    false
}

// ── Individual command handlers ────────────────────────────────────

fn handle_help(transcript: &mut dyn TranscriptHandle) {
    transcript.push_system(
        "Available commands:\n\
         \x20 /help            Show this help\n\
         \x20 /clear           Clear transcript\n\
         \x20 /model <name>    Switch model\n\
         \x20 /compact         Compact conversation\n\
         \x20 /mcp <sub>       Manage MCP servers (list, status, help)\n\
         \x20 /skills          List available skills\n\
         \x20 /quit            Exit"
            .to_string(),
    );
}

fn handle_mcp(subcommand: &str, config: &Config, transcript: &mut dyn TranscriptHandle) {
    match subcommand.trim() {
        "" | "help" => {
            transcript.push_system(
                "MCP server management:\n\
                 \x20 /mcp list       List configured servers\n\
                 \x20 /mcp status     Show server connection status\n\
                 \x20 /mcp help       Show this help"
                    .to_string(),
            );
        }
        "list" => mcp_list(config, transcript),
        "status" => mcp_status(config, transcript),
        other => {
            transcript.push_error(format!(
                "Unknown /mcp subcommand: {other}. Type /mcp help for usage."
            ));
        }
    }
}

fn mcp_list(config: &Config, transcript: &mut dyn TranscriptHandle) {
    if config.mcp_servers.is_empty() {
        transcript.push_system("No MCP servers configured.".to_string());
        return;
    }

    let mut lines = vec!["MCP Servers:".to_string()];
    for (name, server) in &config.mcp_servers {
        let status = if server.disabled {
            "disabled"
        } else {
            "enabled"
        };
        let full_cmd = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };
        lines.push(format!("  {name}  \u{2014} {full_cmd} ({status})"));
    }
    lines.push(String::new());
    lines.push("Use /mcp status to check connection state.".to_string());

    transcript.push_system(lines.join("\n"));
}

fn mcp_status(config: &Config, transcript: &mut dyn TranscriptHandle) {
    // Synchronous placeholder — real status requires async McpManager.
    // Show what we know from config.
    if config.mcp_servers.is_empty() {
        transcript.push_system("No MCP servers configured.".to_string());
        return;
    }

    let mut lines = vec!["MCP Servers (configured):".to_string()];
    for (name, server) in &config.mcp_servers {
        let icon = if server.disabled {
            "\u{2717}"
        } else {
            "\u{2022}"
        };
        let full_cmd = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };
        lines.push(format!("  {icon} {name}  \u{2014} {full_cmd}"));
    }
    lines.push(String::new());
    lines.push(
        "Note: /mcp status shows config state. Run `flown mcp status` for live connection info."
            .to_string(),
    );

    transcript.push_system(lines.join("\n"));
}

fn handle_skills(config: &Config, transcript: &mut dyn TranscriptHandle) {
    let skills_dir = &config.skills_dir;

    // Synchronous read — skill loading is async in core, so we do a lightweight
    // scan here for immediate TUI feedback.
    let mut skills: Vec<(String, String)> = Vec::new();
    collect_skills(skills_dir, &mut skills);

    // Also check .claude/skills in CWD
    if let Ok(cwd) = std::env::current_dir() {
        let local = cwd.join(".claude").join("skills");
        if local.exists() {
            collect_skills(&local, &mut skills);
        }
    }

    if skills.is_empty() {
        transcript.push_system(
            "No skills found.\n\
             \x20 Place skill directories under ~/.flown/skills/<name>/SKILL.md"
                .to_string(),
        );
        return;
    }

    let mut lines = vec!["**Available Skills:**".to_string()];
    for (name, desc) in &skills {
        lines.push(format!("- **{name}** \u{2014} {desc}"));
    }
    lines.push(String::new());
    lines.push(format!("{} skills loaded", skills.len()));

    transcript.push_system(lines.join("\n"));
}

/// Collect skill names and descriptions from a directory by scanning SKILL.md files.
fn collect_skills(dir: &std::path::Path, skills: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_file = path.join("SKILL.md");
            if skill_file.exists() {
                if let Some((name, desc)) = parse_skill_metadata(&skill_file) {
                    skills.push((name, desc));
                }
            }
        }
    }

    skills.sort_by(|a, b| a.0.cmp(&b.0));
}

/// Parse frontmatter from a SKILL.md file to extract name and description.
fn parse_skill_metadata(path: &std::path::Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let (frontmatter, _) = crate::core::skills::parse_frontmatter_static(&content).ok()?;

    let dir_name = path.parent()?.file_name()?.to_string_lossy().to_string();
    let name = frontmatter.get("name").cloned().unwrap_or(dir_name);
    let desc = frontmatter.get("description").cloned().unwrap_or_default();

    if desc.trim().is_empty() {
        return None;
    }

    Some((name, desc))
}
