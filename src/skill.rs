//! `rgx --agent install|uninstall|list|skill`: wire rgx into AI coding agents.
//!
//! An install only writes where rgx owns the namespace (Claude skill dir, Gemini extension), or, for
//! shared files (Codex AGENTS.md, Cursor/VS Code config), edits idempotently — a removable marked
//! block or a merged JSON key — never a blind append. MCP registration that belongs to a host's own
//! CLI is printed, not run. The skill text is version-controlled in `assets/skill.md` and embedded at
//! build time so the installed copy can't drift from the binary (see `CLAUDE.md`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

const SKILL_MD: &str = include_str!("../assets/skill.md");
const VERSION: &str = env!("CARGO_PKG_VERSION");

const BLOCK_BEGIN: &str = "<!-- >>> rgx (managed) >>> -->";
const BLOCK_END: &str = "<!-- <<< rgx (managed) <<< -->";

const CURSOR_DESC: &str = "Prefer rgx over rg/grep/find/fd when searching this repo";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Claude,
    Codex,
    Cursor,
    Gemini,
    VsCode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

impl Target {
    const ALL: [Target; 5] = [
        Target::Claude,
        Target::Codex,
        Target::Cursor,
        Target::Gemini,
        Target::VsCode,
    ];

    fn parse(s: &str) -> Option<Target> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Target::Claude),
            "codex" => Some(Target::Codex),
            "cursor" => Some(Target::Cursor),
            "gemini" | "gemini-cli" => Some(Target::Gemini),
            "vscode" | "vs-code" | "code" | "copilot" => Some(Target::VsCode),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Target::Claude => "Claude Code",
            Target::Codex => "Codex",
            Target::Cursor => "Cursor",
            Target::Gemini => "Gemini CLI",
            Target::VsCode => "VS Code",
        }
    }

    fn default_scope(self) -> Scope {
        match self {
            Target::Claude | Target::Codex | Target::Gemini => Scope::User,
            Target::Cursor | Target::VsCode => Scope::Project,
        }
    }

    fn supports(self, scope: Scope) -> bool {
        !(self == Target::Cursor && scope == Scope::User)
    }
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

/// Filesystem roots, injected so the installer is testable without touching a real `$HOME`.
pub struct Env {
    home: PathBuf,
    cwd: PathBuf,
}

impl Env {
    fn from_system() -> Result<Env> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let cwd = std::env::current_dir().context("current directory")?;
        Ok(Env { home, cwd })
    }

    fn base(&self, scope: Scope) -> &Path {
        match scope {
            Scope::User => &self.home,
            Scope::Project => &self.cwd,
        }
    }
}

struct Report {
    wrote: Vec<PathBuf>,
    notes: Vec<String>,
}

/// `rgx --agent skill`: print the skill document (no side effects).
pub fn print_skill() {
    print!("{SKILL_MD}");
}

/// `rgx --agent install [targets] [--user|--project]`.
pub fn install_cli(args: &[String]) -> Result<()> {
    let (targets, scope) = parse_args(args)?;
    let env = Env::from_system()?;
    let targets = resolve_targets(&targets, &env)?;
    for t in targets {
        let sc = resolve_scope(t, scope)?;
        let r = install_target(&env, t, sc)?;
        print_report(t, sc, &r);
    }
    Ok(())
}

/// `rgx --agent uninstall [targets] [--user|--project]`.
pub fn uninstall_cli(args: &[String]) -> Result<()> {
    let (targets, scope) = parse_args(args)?;
    let env = Env::from_system()?;
    let targets = if targets.is_empty() {
        Target::ALL.to_vec()
    } else {
        targets
    };
    for t in targets {
        let sc = resolve_scope(t, scope)?;
        let removed = uninstall_target(&env, t, sc)?;
        if removed.is_empty() {
            println!("{} ({}): nothing installed", t.label(), sc.label());
        } else {
            println!("{} ({}):", t.label(), sc.label());
            for line in removed {
                println!("  removed {line}");
            }
        }
    }
    Ok(())
}

/// `rgx --agent list`: show each target, whether it's detected, and whether rgx is installed.
pub fn list() -> Result<()> {
    let env = Env::from_system()?;
    for t in Target::ALL {
        let detected = if detect(&env, t) { "detected" } else { "-" };
        let sc = t.default_scope();
        let installed = if is_installed(&env, t, sc) {
            "installed"
        } else {
            "-"
        };
        println!("  {:<12} {:<10} {}", t.label(), detected, installed);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<(Vec<Target>, Option<Scope>)> {
    let mut targets = Vec::new();
    let mut scope = None;
    for a in args {
        match a.as_str() {
            "--user" => scope = Some(Scope::User),
            "--project" | "--repo" => scope = Some(Scope::Project),
            s if s.starts_with('-') => bail!("unknown flag {s:?}"),
            s => targets.push(Target::parse(s).with_context(|| {
                format!("unknown target {s:?} (use: claude, codex, cursor, gemini, vscode)")
            })?),
        }
    }
    Ok((targets, scope))
}

fn resolve_scope(t: Target, scope: Option<Scope>) -> Result<Scope> {
    let sc = scope.unwrap_or_else(|| t.default_scope());
    if !t.supports(sc) {
        bail!("{} supports project scope only", t.label());
    }
    Ok(sc)
}

fn resolve_targets(requested: &[Target], env: &Env) -> Result<Vec<Target>> {
    if !requested.is_empty() {
        return Ok(requested.to_vec());
    }
    let found: Vec<Target> = Target::ALL
        .into_iter()
        .filter(|t| detect(env, *t))
        .collect();
    if found.is_empty() {
        bail!(
            "no agents detected; name one explicitly, e.g. `rgx --agent install claude`\n\
             targets: claude, codex, cursor, gemini, vscode"
        );
    }
    Ok(found)
}

fn detect(env: &Env, t: Target) -> bool {
    match t {
        Target::Claude => env.home.join(".claude").is_dir(),
        Target::Codex => env.home.join(".codex").is_dir(),
        Target::Gemini => env.home.join(".gemini").is_dir(),
        Target::Cursor => env.cwd.join(".cursor").is_dir() || env.home.join(".cursor").is_dir(),
        Target::VsCode => env.cwd.join(".vscode").is_dir() || on_path("code"),
    }
}

fn is_installed(env: &Env, t: Target, scope: Scope) -> bool {
    match t {
        Target::Claude => claude_skill(env, scope).is_file(),
        Target::Gemini => gemini_dir(env, scope)
            .join("gemini-extension.json")
            .is_file(),
        Target::Cursor => env.cwd.join(".cursor/rules/rgx.mdc").is_file(),
        Target::Codex => has_block(&codex_agents(env, scope)),
        Target::VsCode => json_has_rgx(&env.cwd.join(".vscode/mcp.json"), "servers"),
    }
}

fn install_target(env: &Env, t: Target, scope: Scope) -> Result<Report> {
    match t {
        Target::Claude => install_claude(env, scope),
        Target::Codex => install_codex(env, scope),
        Target::Cursor => install_cursor(env),
        Target::Gemini => install_gemini(env, scope),
        Target::VsCode => install_vscode(env, scope),
    }
}

fn install_claude(env: &Env, scope: Scope) -> Result<Report> {
    let path = claude_skill(env, scope);
    write_file(&path, SKILL_MD)?;
    let cmd = match scope {
        Scope::User => "claude mcp add rgx -- rgx --agent mcp",
        Scope::Project => "claude mcp add --scope project rgx -- rgx --agent mcp",
    };
    Ok(Report {
        wrote: vec![path],
        notes: vec![format!("register MCP: {cmd}")],
    })
}

fn install_codex(env: &Env, scope: Scope) -> Result<Report> {
    let path = codex_agents(env, scope);
    upsert_block(&path, skill_body())?;
    Ok(Report {
        wrote: vec![path],
        notes: vec!["register MCP: codex mcp add rgx -- rgx --agent mcp".to_string()],
    })
}

fn install_cursor(env: &Env) -> Result<Report> {
    let rule = env.cwd.join(".cursor/rules/rgx.mdc");
    let body = format!(
        "---\ndescription: {CURSOR_DESC}\nalwaysApply: true\n---\n\n{}",
        skill_body()
    );
    write_file(&rule, &body)?;
    let mcp = env.cwd.join(".cursor/mcp.json");
    merge_mcp_json(&mcp, "mcpServers")?;
    Ok(Report {
        wrote: vec![rule, mcp],
        notes: Vec::new(),
    })
}

fn install_gemini(env: &Env, scope: Scope) -> Result<Report> {
    let dir = gemini_dir(env, scope);
    let manifest = json!({
        "name": "rgx",
        "version": VERSION,
        "mcpServers": { "rgx": rgx_server() },
        "contextFileName": "GEMINI.md",
    });
    let manifest_path = dir.join("gemini-extension.json");
    write_file(&manifest_path, &format!("{}\n", to_pretty(&manifest)?))?;
    let ctx = dir.join("GEMINI.md");
    write_file(&ctx, skill_body())?;
    Ok(Report {
        wrote: vec![manifest_path, ctx],
        notes: Vec::new(),
    })
}

fn install_vscode(env: &Env, scope: Scope) -> Result<Report> {
    match scope {
        Scope::Project => {
            let mcp = env.cwd.join(".vscode/mcp.json");
            merge_mcp_json(&mcp, "servers")?;
            let instr = env.cwd.join(".github/copilot-instructions.md");
            upsert_block(&instr, skill_body())?;
            Ok(Report {
                wrote: vec![mcp, instr],
                notes: Vec::new(),
            })
        }
        Scope::User => Ok(Report {
            wrote: Vec::new(),
            notes: vec![
                "register MCP: code --add-mcp \
                 '{\"name\":\"rgx\",\"command\":\"rgx\",\"args\":[\"--agent\",\"mcp\"]}'"
                    .to_string(),
                "add the skill to your user copilot-instructions in VS Code settings".to_string(),
            ],
        }),
    }
}

fn uninstall_target(env: &Env, t: Target, scope: Scope) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    match t {
        Target::Claude => remove_file_into(&claude_skill(env, scope), &mut removed),
        Target::Gemini => {
            let dir = gemini_dir(env, scope);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("remove {}", dir.display()))?;
                removed.push(dir.display().to_string());
            }
        }
        Target::Cursor => {
            remove_file_into(&env.cwd.join(".cursor/rules/rgx.mdc"), &mut removed);
            remove_mcp_json(
                &env.cwd.join(".cursor/mcp.json"),
                "mcpServers",
                &mut removed,
            )?;
        }
        Target::Codex => remove_block_into(&codex_agents(env, scope), &mut removed)?,
        Target::VsCode => {
            remove_mcp_json(&env.cwd.join(".vscode/mcp.json"), "servers", &mut removed)?;
            remove_block_into(
                &env.cwd.join(".github/copilot-instructions.md"),
                &mut removed,
            )?;
        }
    }
    Ok(removed)
}

fn claude_skill(env: &Env, scope: Scope) -> PathBuf {
    env.base(scope).join(".claude/skills/rgx/SKILL.md")
}

fn codex_agents(env: &Env, scope: Scope) -> PathBuf {
    match scope {
        Scope::User => env.home.join(".codex/AGENTS.md"),
        Scope::Project => env.cwd.join("AGENTS.md"),
    }
}

fn gemini_dir(env: &Env, scope: Scope) -> PathBuf {
    env.base(scope).join(".gemini/extensions/rgx")
}

fn rgx_server() -> Value {
    json!({ "command": "rgx", "args": ["--agent", "mcp"] })
}

fn skill_body() -> &'static str {
    if let Some(rest) = SKILL_MD.strip_prefix("---\n")
        && let Some(idx) = rest.find("\n---\n")
    {
        return rest[idx + 5..].trim_start_matches('\n');
    }
    SKILL_MD
}

fn print_report(t: Target, scope: Scope, r: &Report) {
    println!("{} ({}):", t.label(), scope.label());
    for p in &r.wrote {
        println!("  wrote   {}", p.display());
    }
    for note in &r.notes {
        println!("  {note}");
    }
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

fn remove_file_into(path: &Path, removed: &mut Vec<String>) {
    if path.is_file() && std::fs::remove_file(path).is_ok() {
        removed.push(path.display().to_string());
    }
}

fn to_pretty(v: &Value) -> Result<String> {
    serde_json::to_string_pretty(v).context("serialize JSON")
}

fn merge_mcp_json(path: &Path, root_key: &str) -> Result<()> {
    let mut root = read_json(path)?;
    let obj = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;
    let servers = obj
        .entry(root_key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("{root_key} in {} is not an object", path.display()))?;
    servers.insert("rgx".to_string(), rgx_server());
    write_file(path, &format!("{}\n", to_pretty(&root)?))
}

fn remove_mcp_json(path: &Path, root_key: &str, removed: &mut Vec<String>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut root = read_json(path)?;
    let gone = root
        .as_object_mut()
        .and_then(|o| o.get_mut(root_key))
        .and_then(|s| s.as_object_mut())
        .map(|s| s.remove("rgx").is_some())
        .unwrap_or(false);
    if gone {
        write_file(path, &format!("{}\n", to_pretty(&root)?))?;
        removed.push(format!("{} (rgx key)", path.display()));
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

fn json_has_rgx(path: &Path, root_key: &str) -> bool {
    read_json(path)
        .ok()
        .and_then(|v| v.get(root_key).and_then(|s| s.get("rgx")).map(|_| ()))
        .is_some()
}

fn block_text(body: &str) -> String {
    format!("{BLOCK_BEGIN}\n{}\n{BLOCK_END}\n", body.trim())
}

fn upsert_block(path: &Path, body: &str) -> Result<()> {
    let existing = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else {
        String::new()
    };
    let block = block_text(body);
    let new = match find_block(&existing) {
        Some((s, e)) => format!("{}{}{}", &existing[..s], block, &existing[e..]),
        None if existing.trim().is_empty() => block,
        None => format!("{}\n\n{}", existing.trim_end(), block),
    };
    write_file(path, &new)
}

fn remove_block_into(path: &Path, removed: &mut Vec<String>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let existing =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if let Some((s, e)) = find_block(&existing) {
        let trimmed = format!("{}{}", &existing[..s], &existing[e..]);
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
        } else {
            write_file(path, &format!("{trimmed}\n"))?;
        }
        removed.push(format!("{} (rgx block)", path.display()));
    }
    Ok(())
}

fn has_block(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| find_block(&s).is_some())
        .unwrap_or(false)
}

fn find_block(s: &str) -> Option<(usize, usize)> {
    let start = s.find(BLOCK_BEGIN)?;
    let end_marker = s[start..].find(BLOCK_END)? + start + BLOCK_END.len();
    let end = s[end_marker..]
        .find('\n')
        .map(|n| end_marker + n + 1)
        .unwrap_or(end_marker);
    Some((start, end))
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|dir| dir.join(bin).is_file() || dir.join(format!("{bin}.exe")).is_file())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = dir.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let env = Env { home, cwd };
        (dir, env)
    }

    #[test]
    fn installs_every_target_into_its_own_namespace() {
        let (_d, env) = temp_env();
        for t in Target::ALL {
            let scope = t.default_scope();
            install_target(&env, t, scope).unwrap();
        }
        assert!(env.home.join(".claude/skills/rgx/SKILL.md").is_file());
        assert!(
            env.home
                .join(".gemini/extensions/rgx/gemini-extension.json")
                .is_file()
        );
        assert!(env.home.join(".gemini/extensions/rgx/GEMINI.md").is_file());
        assert!(env.cwd.join(".cursor/rules/rgx.mdc").is_file());
        assert!(env.cwd.join(".cursor/mcp.json").is_file());
        assert!(has_block(&env.home.join(".codex/AGENTS.md")));
        assert!(env.cwd.join(".vscode/mcp.json").is_file());
        assert!(has_block(&env.cwd.join(".github/copilot-instructions.md")));

        let mdc = std::fs::read_to_string(env.cwd.join(".cursor/rules/rgx.mdc")).unwrap();
        assert!(mdc.starts_with("---\n"));
        assert!(mdc.contains("alwaysApply: true"));
    }

    #[test]
    fn merge_preserves_existing_servers_and_is_idempotent() {
        let (_d, env) = temp_env();
        let mcp = env.cwd.join(".vscode/mcp.json");
        write_file(
            &mcp,
            "{\n  \"servers\": { \"other\": { \"command\": \"x\" } }\n}\n",
        )
        .unwrap();
        merge_mcp_json(&mcp, "servers").unwrap();
        merge_mcp_json(&mcp, "servers").unwrap();
        let v = read_json(&mcp).unwrap();
        assert!(v["servers"]["other"].is_object());
        assert_eq!(v["servers"]["rgx"]["command"], "rgx");
    }

    #[test]
    fn block_upsert_is_idempotent_and_preserves_surrounding_text() {
        let (_d, env) = temp_env();
        let path = env.cwd.join("AGENTS.md");
        write_file(&path, "# Project\n\nHand-written notes.\n").unwrap();
        upsert_block(&path, "first").unwrap();
        upsert_block(&path, "second").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(BLOCK_BEGIN).count(), 1);
        assert!(text.contains("Hand-written notes."));
        assert!(text.contains("second"));
        assert!(!text.contains("first"));
    }

    #[test]
    fn uninstall_removes_block_and_json_key_but_keeps_user_content() {
        let (_d, env) = temp_env();
        install_target(&env, Target::Codex, Scope::User).unwrap();
        let agents = codex_agents(&env, Scope::User);
        std::fs::write(
            &agents,
            format!("# Mine\n\n{}", std::fs::read_to_string(&agents).unwrap()),
        )
        .unwrap();
        let removed = uninstall_target(&env, Target::Codex, Scope::User).unwrap();
        assert!(!removed.is_empty());
        let text = std::fs::read_to_string(&agents).unwrap();
        assert!(text.contains("# Mine"));
        assert!(!text.contains(BLOCK_BEGIN));

        install_target(&env, Target::VsCode, Scope::Project).unwrap();
        uninstall_target(&env, Target::VsCode, Scope::Project).unwrap();
        assert!(!json_has_rgx(&env.cwd.join(".vscode/mcp.json"), "servers"));
    }

    #[test]
    fn cursor_rejects_user_scope() {
        assert!(resolve_scope(Target::Cursor, Some(Scope::User)).is_err());
        assert!(resolve_scope(Target::Cursor, None).is_ok());
    }
}
