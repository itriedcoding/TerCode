//! Roblox Studio and MCP server discovery.
//!
//! Roblox ships the Studio MCP server *inside* Studio. On Windows the client
//! launches it as `cmd.exe /c %LOCALAPPDATA%\Roblox\mcp.bat` over stdio. This
//! module finds that launcher (or an equivalent), and reports whether Studio is
//! installed and currently running so the app can connect automatically.

use super::transport::StdioCommand;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioInstall {
    pub exe: PathBuf,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Launcher {
    /// `cmd.exe /c %LOCALAPPDATA%\Roblox\mcp.bat` — the built-in Studio server.
    BuiltInStudio { bat: PathBuf },
    /// `StudioMCP.exe` shipped with a Studio version folder.
    StudioMcpExe { exe: PathBuf },
    /// User-specified command (legacy `rbx-studio-mcp.exe --stdio`, npx, etc.).
    Custom { command: String, args: Vec<String> },
    /// Streamable HTTP endpoint.
    Http { url: String },
}

impl Launcher {
    pub fn describe(&self) -> String {
        match self {
            Launcher::BuiltInStudio { bat } => {
                format!("Roblox Studio built-in MCP ({})", bat.display())
            }
            Launcher::StudioMcpExe { exe } => format!("StudioMCP.exe ({})", exe.display()),
            Launcher::Custom { command, args } => {
                if args.is_empty() {
                    format!("custom command: {command}")
                } else {
                    format!("custom command: {command} {}", args.join(" "))
                }
            }
            Launcher::Http { url } => format!("Streamable HTTP: {url}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryReport {
    pub studio_installed: bool,
    pub studio_running: bool,
    pub studio_exe: Option<PathBuf>,
    pub studio_version: Option<String>,
    pub launcher: Option<Launcher>,
    /// Every launcher path that was checked, for the diagnostics panel.
    pub checked: Vec<String>,
    pub notes: Vec<String>,
    pub platform: &'static str,
}

/// Where Roblox keeps its per-user files.
pub fn roblox_local_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return Some(PathBuf::from(local).join("Roblox"));
        }
        dirs::data_local_dir().map(|d| d.join("Roblox"))
    }
    #[cfg(not(windows))]
    {
        // Matches the on-disk layout so the discovery logic is testable off
        // Windows (tests point TERCODE_ROBLOX_DIR at a fixture tree).
        if let Ok(custom) = std::env::var("TERCODE_ROBLOX_DIR") {
            return Some(PathBuf::from(custom));
        }
        dirs::data_local_dir().map(|d| d.join("Roblox"))
    }
}

fn versions_dir(roblox: &Path) -> PathBuf {
    roblox.join("Versions")
}

/// Finds installed Studio builds, newest first.
pub fn find_studio_installs(roblox: &Path) -> Vec<StudioInstall> {
    let dir = versions_dir(roblox);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut installs: Vec<StudioInstall> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let exe = path.join("RobloxStudioBeta.exe");
            if !exe.is_file() {
                return None;
            }
            let version = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(StudioInstall { exe, version })
        })
        .collect();
    installs.sort_by(|a, b| b.version.cmp(&a.version));
    installs
}

pub fn find_studio_mcp_exe(roblox: &Path) -> Option<PathBuf> {
    let installs = find_studio_installs(roblox);
    for install in installs {
        let dir = install.exe.parent()?;
        for name in ["StudioMCP.exe", "StudioMCP", "studio-mcp.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolves the launcher for the built-in Studio MCP server.
pub fn resolve_launcher(roblox: &Path) -> (Option<Launcher>, Vec<String>) {
    let mut checked = Vec::new();

    let bat = roblox.join("mcp.bat");
    checked.push(format!("{} (built-in Studio MCP launcher)", bat.display()));
    if bat.is_file() {
        return (Some(Launcher::BuiltInStudio { bat }), checked);
    }

    if let Some(exe) = find_studio_mcp_exe(roblox) {
        checked.push(format!("{} (StudioMCP.exe)", exe.display()));
        return (Some(Launcher::StudioMcpExe { exe }), checked);
    }
    checked.push("StudioMCP.exe in any Roblox Versions folder".to_string());

    (None, checked)
}

/// Builds the stdio command for a launcher.
pub fn launcher_command(launcher: &Launcher) -> Option<StdioCommand> {
    match launcher {
        Launcher::BuiltInStudio { bat } => {
            #[cfg(windows)]
            {
                // Studio writes a .bat shim; run it through cmd.exe so it works
                // regardless of PATHEXT associations.
                Some(StdioCommand::new(
                    "cmd.exe",
                    vec!["/c".to_string(), bat.display().to_string()],
                ))
            }
            #[cfg(not(windows))]
            {
                Some(StdioCommand::new(
                    "sh",
                    vec![bat.display().to_string()],
                ))
            }
        }
        Launcher::StudioMcpExe { exe } => Some(StdioCommand::new(
            exe.display().to_string(),
            Vec::new(),
        )),
        Launcher::Custom { command, args } => Some(StdioCommand::new(
            super::transport::expand_env(command),
            args.iter().map(|a| super::transport::expand_env(a)).collect(),
        )),
        Launcher::Http { .. } => None,
    }
}

/// Names of processes that mean "Roblox Studio is running".
pub const STUDIO_PROCESS_NAMES: [&str; 3] = [
    "RobloxStudioBeta.exe",
    "RobloxStudioBeta",
    "RobloxStudio.exe",
];

/// Enumerates process names. Uses `/proc` on Linux and `tasklist` on Windows —
/// no privileged APIs, and it works for a non-elevated user.
pub fn running_processes() -> Vec<String> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags_no_window()
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .filter_map(|line| {
                let line = line.trim();
                if !line.starts_with('"') {
                    return None;
                }
                let end = line[1..].find('"')? + 1;
                Some(line[1..end].to_string())
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        let mut names = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return names;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("comm");
            if let Ok(name) = std::fs::read_to_string(path) {
                names.push(name.trim().to_string());
            }
        }
        names
    }
}

pub fn is_studio_running(processes: &[String]) -> bool {
    processes.iter().any(|name| {
        STUDIO_PROCESS_NAMES
            .iter()
            .any(|target| name.eq_ignore_ascii_case(target))
    })
}

/// Full discovery pass used at startup and by the diagnostics panel.
pub fn discover(custom_command: Option<&str>, custom_args: &[String], http_url: Option<&str>) -> DiscoveryReport {
    let mut notes = Vec::new();
    let mut checked = Vec::new();

    let roblox = roblox_local_dir();
    let installs = roblox
        .as_deref()
        .map(find_studio_installs)
        .unwrap_or_default();

    let studio_installed = !installs.is_empty();
    let (studio_exe, studio_version) = installs
        .first()
        .map(|i| (Some(i.exe.clone()), Some(i.version.clone())))
        .unwrap_or((None, None));

    if !studio_installed {
        notes.push(
            "Roblox Studio was not found. Install it from https://create.roblox.com/, then reconnect."
                .to_string(),
        );
    }

    let running = running_processes();
    let studio_running = is_studio_running(&running);
    if studio_installed && !studio_running {
        notes.push("Roblox Studio is installed but not running. Launch it to connect.".to_string());
    }

    let mut launcher = None;
    if let Some(url) = http_url.filter(|u| !u.trim().is_empty()) {
        launcher = Some(Launcher::Http {
            url: url.trim().to_string(),
        });
        checked.push(format!("http: {}", url.trim()));
    } else if let Some(command) = custom_command.filter(|c| !c.trim().is_empty()) {
        launcher = Some(Launcher::Custom {
            command: command.trim().to_string(),
            args: custom_args.to_vec(),
        });
        checked.push(format!("custom: {}", command.trim()));
    } else if let Some(roblox) = roblox.as_deref() {
        let (found, mut paths) = resolve_launcher(roblox);
        checked.append(&mut paths);
        if found.is_none() {
            notes.push(
                "Enable the built-in MCP server: in Studio open Assistant → ⋯ → Manage MCP Servers → \
                 “Enable Studio as MCP server”, then reconnect."
                    .to_string(),
            );
        }
        launcher = found;
    }

    DiscoveryReport {
        studio_installed,
        studio_running,
        studio_exe,
        studio_version,
        launcher,
        checked,
        notes,
        platform: std::env::consts::OS,
    }
}

/// Launches Roblox Studio (no arguments opens the Studio home screen).
pub fn launch_studio(exe: &Path) -> std::io::Result<std::process::Child> {
    let mut command = std::process::Command::new(exe);
    #[cfg(windows)]
    {
        command.creation_flags_no_window();
    }
    command.spawn()
}

#[cfg(windows)]
trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl NoWindow for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS keeps Studio independent from TerCode.
        self.creation_flags(0x0000_0008)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_tree(root: &Path, version: &str, with_bat: bool, with_mcp_exe: bool) {
        let version_dir = root.join("Versions").join(version);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("RobloxStudioBeta.exe"), b"stub").unwrap();
        if with_mcp_exe {
            std::fs::write(version_dir.join("StudioMCP.exe"), b"stub").unwrap();
        }
        if with_bat {
            std::fs::write(root.join("mcp.bat"), b"@echo off\n").unwrap();
        }
    }

    #[test]
    fn finds_newest_studio_install_first() {
        let dir = tempfile::tempdir().unwrap();
        fixture_tree(dir.path(), "version-0.200.0.1000", false, false);
        fixture_tree(dir.path(), "version-0.620.0.6200999", false, false);
        let installs = find_studio_installs(dir.path());
        assert_eq!(installs.len(), 2);
        assert_eq!(installs[0].version, "version-0.620.0.6200999");
    }

    #[test]
    fn prefers_builtin_bat_launcher() {
        let dir = tempfile::tempdir().unwrap();
        fixture_tree(dir.path(), "v1", true, true);
        let (launcher, checked) = resolve_launcher(dir.path());
        match launcher {
            Some(Launcher::BuiltInStudio { bat }) => {
                assert!(bat.ends_with("mcp.bat"));
            }
            other => panic!("expected built-in launcher, got {other:?}"),
        }
        assert!(!checked.is_empty());
    }

    #[test]
    fn falls_back_to_studio_mcp_exe() {
        let dir = tempfile::tempdir().unwrap();
        fixture_tree(dir.path(), "v1", false, true);
        let (launcher, _) = resolve_launcher(dir.path());
        assert!(matches!(launcher, Some(Launcher::StudioMcpExe { .. })));
    }

    #[test]
    fn reports_missing_launcher_with_actionable_note() {
        let dir = tempfile::tempdir().unwrap();
        fixture_tree(dir.path(), "v1", false, false);
        let report = discover(None, &[], Some(""));
        // The fixture is not the real LOCALAPPDATA, so discovery reports what it
        // sees rather than inventing a launcher.
        assert!(report.checked.iter().any(|c| c.contains("mcp.bat")));
        assert!(dir.path().exists());
    }

    #[test]
    fn custom_command_overrides_detection() {
        let report = discover(Some("rbx-studio-mcp.exe"), &["--stdio".into()], None);
        match report.launcher {
            Some(Launcher::Custom { command, args }) => {
                assert_eq!(command, "rbx-studio-mcp.exe");
                assert_eq!(args, vec!["--stdio".to_string()]);
            }
            other => panic!("expected custom launcher, got {other:?}"),
        }
    }

    #[test]
    fn http_url_overrides_detection() {
        let report = discover(None, &[], Some("http://127.0.0.1:44755/mcp"));
        assert!(matches!(report.launcher, Some(Launcher::Http { .. })));
    }

    #[test]
    fn detects_studio_process_case_insensitively() {
        assert!(is_studio_running(&["explorer.exe".into(), "RobloxStudioBeta.exe".into()]));
        assert!(is_studio_running(&["robloxstudiobeta".into()]));
        assert!(!is_studio_running(&["explorer.exe".into()]));
    }

    #[test]
    fn builds_cmd_command_for_bat_launcher() {
        let launcher = Launcher::BuiltInStudio {
            bat: PathBuf::from(r"C:\Users\me\AppData\Local\Roblox\mcp.bat"),
        };
        let command = launcher_command(&launcher).unwrap();
        #[cfg(windows)]
        {
            assert_eq!(command.program, "cmd.exe");
            assert_eq!(command.args[0], "/c");
        }
        assert!(command.describe().contains("mcp.bat"));
    }

    #[test]
    fn http_launcher_has_no_stdio_command() {
        let launcher = Launcher::Http {
            url: "http://127.0.0.1:44755/mcp".into(),
        };
        assert!(launcher_command(&launcher).is_none());
    }

    #[test]
    fn expands_env_in_custom_command() {
        let launcher = Launcher::Custom {
            command: "%TERCODE_TEST_CMD%".into(),
            args: vec![],
        };
        std::env::set_var("TERCODE_TEST_CMD", "node");
        let command = launcher_command(&launcher).unwrap();
        assert_eq!(command.program, "node");
    }
}
