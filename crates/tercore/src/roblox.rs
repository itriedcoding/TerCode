//! Roblox project integration: place/XML parsing, model export, Rojo builds.

use crate::error::{Result, TerError};
use crate::project::safe_join;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A node in a place/model hierarchy read from `.rbxlx`/`.rbxmx` XML.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstanceNode {
    pub class_name: String,
    pub name: String,
    pub children: Vec<InstanceNode>,
}

impl InstanceNode {
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(|c| c.count()).sum::<usize>()
    }

    pub fn find(&self, name: &str) -> Option<&InstanceNode> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceSummary {
    pub path: PathBuf,
    pub format: String,
    pub roots: Vec<InstanceNode>,
    pub instance_count: usize,
    pub scripts: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceFormat {
    Rbxlx,
    Rbxmx,
    Rbxl,
    Rbxm,
    Unknown,
}

impl PlaceFormat {
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "rbxlx" => PlaceFormat::Rbxlx,
            "rbxmx" => PlaceFormat::Rbxmx,
            "rbxl" => PlaceFormat::Rbxl,
            "rbxm" => PlaceFormat::Rbxm,
            _ => PlaceFormat::Unknown,
        }
    }

    pub fn is_xml(self) -> bool {
        matches!(self, PlaceFormat::Rbxlx | PlaceFormat::Rbxmx)
    }

    pub fn label(self) -> &'static str {
        match self {
            PlaceFormat::Rbxlx => "rbxlx (XML place)",
            PlaceFormat::Rbxmx => "rbxmx (XML model)",
            PlaceFormat::Rbxl => "rbxl (binary place)",
            PlaceFormat::Rbxm => "rbxm (binary model)",
            PlaceFormat::Unknown => "unknown",
        }
    }
}

/// Parses an XML place/model file into an instance tree.
pub fn parse_place_xml(xml: &str) -> Result<Vec<InstanceNode>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut roots: Vec<InstanceNode> = Vec::new();
    let mut stack: Vec<InstanceNode> = Vec::new();
    let mut pending_name = String::new();
    let mut in_name_string = false;

    loop {
        match reader.read_event() {
            Err(e) => {
                return Err(TerError::Invalid(format!(
                    "could not parse Roblox XML at byte {}: {e}",
                    reader.buffer_position()
                )))
            }
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                open_element(&e, false, &mut stack, &mut in_name_string, &mut pending_name)
            }
            Ok(Event::Empty(e)) => {
                // <Item/> has no body: open and close it in one step.
                open_element(&e, true, &mut stack, &mut in_name_string, &mut pending_name);
                close_item(&mut stack, &mut roots);
            }
            Ok(Event::Text(e)) => {
                if in_name_string {
                    if let Ok(text) = e.unescape() {
                        pending_name.push_str(&text);
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if in_name_string {
                    pending_name.push_str(&String::from_utf8_lossy(&e));
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "string" && in_name_string {
                    in_name_string = false;
                    let name = std::mem::take(&mut pending_name);
                    if let Some(node) = stack.last_mut() {
                        if node.name.is_empty() {
                            node.name = name;
                        }
                    }
                } else if tag == "Item" {
                    close_item(&mut stack, &mut roots);
                }
            }
            _ => {}
        }
    }

    if roots.is_empty() {
        return Err(TerError::Invalid(
            "no <Item> elements found; this does not look like a Roblox XML place/model".into(),
        ));
    }
    Ok(roots)
}

/// Reads a place or model file and summarises it.
pub fn summarize_place(path: &Path) -> Result<PlaceSummary> {
    let format = PlaceFormat::from_path(path);
    let bytes = std::fs::read(path).map_err(|e| TerError::io(path.display().to_string(), e))?;
    let is_binary_roblox = bytes.starts_with(b"<roblox!") || bytes.starts_with(b"<roblox");
    let summary = |roots: Vec<InstanceNode>, note: Option<String>| {
        let count: usize = roots.iter().map(|r| r.count()).sum();
        let mut scripts = Vec::new();
        for root in &roots {
            collect_scripts(root, &mut scripts);
        }
        PlaceSummary {
            path: path.to_path_buf(),
            format: format.label().to_string(),
            roots,
            instance_count: count,
            scripts,
            note,
        }
    };

    if format.is_xml() || (!is_binary_roblox && looks_like_xml(&bytes)) {
        let text = String::from_utf8_lossy(&bytes).to_string();
        let roots = parse_place_xml(&text)?;
        return Ok(summary(roots, None));
    }

    if is_binary_roblox || matches!(format, PlaceFormat::Rbxl | PlaceFormat::Rbxm) {
        // Binary place files are not reverse-engineered here: Studio and Rojo
        // are the supported path for them.
        return Ok(summary(
            Vec::new(),
            Some(
                "Binary Roblox files are opened in Studio. Use Rojo (`rojo build`) to convert a \
                 project to .rbxlx/.rbxl, or connect Studio through MCP to inspect it live."
                    .into(),
            ),
        ));
    }

    Err(TerError::Invalid(format!(
        "{} is not a Roblox place or model file",
        path.display()
    )))
}

fn open_element(
    element: &BytesStart<'_>,
    self_closing: bool,
    stack: &mut Vec<InstanceNode>,
    in_name_string: &mut bool,
    pending_name: &mut String,
) {
    let tag = String::from_utf8_lossy(element.name().as_ref()).to_string();
    if tag == "Item" {
        let mut class = String::from("Instance");
        for attr in element.attributes().flatten() {
            if attr.key.as_ref() == b"class" {
                class = String::from_utf8_lossy(&attr.value).to_string();
            }
        }
        stack.push(InstanceNode {
            class_name: class,
            name: String::new(),
            children: Vec::new(),
        });
    } else if tag == "string" && !self_closing {
        let mut prop_name = String::new();
        for attr in element.attributes().flatten() {
            if attr.key.as_ref() == b"name" {
                prop_name = String::from_utf8_lossy(&attr.value).to_string();
            }
        }
        if prop_name == "Name" {
            *in_name_string = true;
            pending_name.clear();
        }
    }
}

fn close_item(stack: &mut Vec<InstanceNode>, roots: &mut Vec<InstanceNode>) {
    if let Some(mut node) = stack.pop() {
        if node.name.is_empty() {
            node.name = node.class_name.clone();
        }
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => roots.push(node),
        }
    }
}

fn looks_like_xml(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
    head.trim_start().starts_with("<?xml") || head.contains("<roblox")
}

fn collect_scripts(node: &InstanceNode, out: &mut Vec<String>) {
    if matches!(
        node.class_name.as_str(),
        "Script" | "LocalScript" | "ModuleScript"
    ) {
        out.push(node.name.clone());
    }
    for child in &node.children {
        collect_scripts(child, out);
    }
}

/// Maps a Luau file path to the Roblox script class Rojo would produce.
pub fn script_class_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".server.luau") || lower.ends_with(".server.lua") {
        "Script"
    } else if lower.ends_with(".client.luau") || lower.ends_with(".client.lua") {
        "LocalScript"
    } else {
        "ModuleScript"
    }
}

/// `src/server/Main.server.luau` → `Main` (the name Studio shows).
fn script_display_name(path: &str) -> Option<String> {
    let file = Path::new(path).file_name()?.to_string_lossy().to_string();
    let stripped = file
        .trim_end_matches(".luau")
        .trim_end_matches(".lua")
        .trim_end_matches(".server")
        .trim_end_matches(".client")
        .to_string();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped)
    }
}

/// Escapes text for XML and splits CDATA terminators.
fn cdata(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (i, chunk) in text.split("]]>").enumerate() {
        if i > 0 {
            out.push_str("]]]]><![CDATA[>");
        }
        out.push_str(chunk);
    }
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Exports a project's Luau files as a `.rbxmx` model that can be dropped into
/// Studio (File → Insert from File).
pub fn export_model(project_root: &Path, files: &[(String, String)], model_name: &str) -> Result<String> {
    let _ = project_root;
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<roblox version=\"4\">\n");
    let mut referent = 0usize;
    xml.push_str(&format!(
        "  <Item class=\"Folder\" referent=\"RBX{}\">\n    <Properties>\n      <string name=\"Name\">{}</string>\n    </Properties>\n",
        referent,
        xml_escape(model_name)
    ));
    for (index, (path, contents)) in files.iter().enumerate() {
        referent += 1;
        let class = script_class_for(path);
        let name = script_display_name(path).unwrap_or_else(|| format!("Script{}", index + 1));
        xml.push_str(&format!(
            "    <Item class=\"{class}\" referent=\"RBX{}\">\n      <Properties>\n        <string name=\"Name\">{}</string>\n        <ProtectedString name=\"Source\"><![CDATA[{}]]></ProtectedString>\n      </Properties>\n    </Item>\n",
            referent,
            xml_escape(&name),
            cdata(contents)
        ));
    }
    xml.push_str("  </Item>\n</roblox>\n");
    Ok(xml)
}

/// Writes a `.rbxmx` model for the given project files.
pub fn export_model_file(
    project_root: &Path,
    files: &[(String, String)],
    model_name: &str,
    output: &Path,
) -> Result<PathBuf> {
    let xml = export_model(project_root, files, model_name)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| TerError::io(parent.display().to_string(), e))?;
    }
    std::fs::write(output, xml).map_err(|e| TerError::io(output.display().to_string(), e))?;
    Ok(output.to_path_buf())
}

/// Reads every Luau source file in a project.
pub fn collect_luau_files(root: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| TerError::io(dir.display().to_string(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_luau = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("luau") || e.eq_ignore_ascii_case("lua"))
                .unwrap_or(false);
            if is_luau {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    out.push((crate::project::relative(root, &path), contents));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Locates the `rojo` CLI on PATH.
pub fn find_rojo() -> Option<PathBuf> {
    let exe_names: [&str; 2] = if cfg!(windows) {
        ["rojo.exe", "rojo"]
    } else {
        ["rojo", "rojo.exe"]
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for exe in exe_names {
                let candidate = dir.join(exe);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    // Common cargo install location.
    if let Some(home) = dirs::home_dir() {
        for exe in exe_names {
            let candidate = home.join(".cargo").join("bin").join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoStatus {
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

pub fn rojo_status() -> RojoStatus {
    let Some(path) = find_rojo() else {
        return RojoStatus {
            installed: false,
            path: None,
            version: None,
        };
    };
    let version = Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    RojoStatus {
        installed: true,
        path: Some(path),
        version,
    }
}

/// Runs `rojo build` for a project and returns the produced place file.
pub fn rojo_build(project_root: &Path, output: &Path) -> Result<PathBuf> {
    let rojo = find_rojo().ok_or_else(|| {
        TerError::Invalid(
            "Rojo is not installed. Install it with `cargo install rojo` or from rojo.space, then retry."
                .into(),
        )
    })?;
    let project_file = ["default.project.json", "project.json"]
        .iter()
        .map(|name| project_root.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            TerError::Invalid(
                "No default.project.json found. Generate one from the project menu first.".into(),
            )
        })?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| TerError::io(parent.display().to_string(), e))?;
    }
    let result = Command::new(&rojo)
        .current_dir(project_root)
        .arg("build")
        .arg(&project_file)
        .arg("--output")
        .arg(output)
        .output()
        .map_err(|e| TerError::io(rojo.display().to_string(), e))?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(TerError::Invalid(format!(
            "rojo build failed: {}",
            stderr.trim().chars().take(500).collect::<String>()
        )));
    }
    Ok(output.to_path_buf())
}

/// Writes a Rojo project file for a project that does not have one.
pub fn ensure_rojo_project(project_root: &Path, name: &str) -> Result<PathBuf> {
    let target = safe_join(project_root, "default.project.json")?;
    if target.is_file() {
        return Ok(target);
    }
    let json = format!(
        r#"{{
  "name": "{}",
  "tree": {{
    "$className": "DataModel",
    "ServerScriptService": {{ "$className": "ServerScriptService", "$path": "src/server" }},
    "StarterPlayer": {{
      "$className": "StarterPlayer",
      "StarterPlayerScripts": {{ "$className": "StarterPlayerScripts", "$path": "src/client" }}
    }},
    "ReplicatedStorage": {{ "$className": "ReplicatedStorage", "$path": "src/shared" }}
  }}
}}
"#,
        name.replace('"', "")
    );
    std::fs::write(&target, json).map_err(|e| TerError::io(target.display().to_string(), e))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<roblox version="4">
  <Item class="Workspace" referent="RBX0">
    <Properties>
      <string name="Name">Workspace</string>
    </Properties>
    <Item class="Part" referent="RBX1">
      <Properties>
        <string name="Name">Baseplate</string>
      </Properties>
    </Item>
    <Item class="Script" referent="RBX2">
      <Properties>
        <string name="Name">GameLoop</string>
        <ProtectedString name="Source"><![CDATA[print("hi")]]></ProtectedString>
      </Properties>
    </Item>
  </Item>
  <Item class="ServerScriptService" referent="RBX3">
    <Properties>
      <string name="Name">ServerScriptService</string>
    </Properties>
    <Item class="ModuleScript" referent="RBX4">
      <Properties>
        <string name="Name">Config</string>
      </Properties>
    </Item>
  </Item>
</roblox>
"#;

    #[test]
    fn parses_place_hierarchy() {
        let roots = parse_place_xml(SAMPLE).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].class_name, "Workspace");
        assert_eq!(roots[0].children.len(), 2);
        assert_eq!(roots[0].children[0].name, "Baseplate");
        assert_eq!(roots[0].count(), 3);
        assert_eq!(roots[1].children[0].class_name, "ModuleScript");
    }

    #[test]
    fn finds_nested_instances() {
        let roots = parse_place_xml(SAMPLE).unwrap();
        let script = roots[0].find("GameLoop").unwrap();
        assert_eq!(script.class_name, "Script");
    }

    #[test]
    fn rejects_non_place_xml() {
        let err = parse_place_xml("<html><body>nope</body></html>").unwrap_err();
        assert!(err.to_string().contains("no <Item>"));
    }

    #[test]
    fn summarises_place_with_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MyGame.rbxlx");
        std::fs::write(&path, SAMPLE).unwrap();
        let summary = summarize_place(&path).unwrap();
        assert_eq!(summary.instance_count, 5);
        assert!(summary.scripts.contains(&"GameLoop".to_string()));
        assert!(summary.scripts.contains(&"Config".to_string()));
        assert!(summary.note.is_none());
    }

    #[test]
    fn binary_place_is_reported_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Game.rbxl");
        std::fs::write(&path, b"<roblox! \x00\x01binary").unwrap();
        let summary = summarize_place(&path).unwrap();
        assert!(summary.roots.is_empty());
        assert!(summary.note.unwrap().contains("Studio"));
    }

    #[test]
    fn maps_script_classes() {
        assert_eq!(script_class_for("src/server/Main.server.luau"), "Script");
        assert_eq!(script_class_for("src/client/Hud.client.luau"), "LocalScript");
        assert_eq!(script_class_for("src/shared/Config.luau"), "ModuleScript");
    }

    #[test]
    fn exports_valid_rbxmx_model() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![
            ("src/server/Main.server.luau".to_string(), "print('server')".to_string()),
            ("src/shared/Config.luau".to_string(), "return {}".to_string()),
        ];
        let path = export_model_file(dir.path(), &files, "MyGame", &dir.path().join("out/MyGame.rbxmx"))
            .unwrap();
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(xml.contains("class=\"Script\""));
        assert!(xml.contains("class=\"ModuleScript\""));
        assert!(xml.contains("<![CDATA[print('server')]]>"));
        // The generated XML must round-trip through our own parser.
        let roots = parse_place_xml(&xml).unwrap();
        assert_eq!(roots[0].name, "MyGame");
        assert_eq!(roots[0].children.len(), 2);
    }

    #[test]
    fn cdata_escaping_survives_terminator() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![("src/a.luau".to_string(), "local s = ']]>'\nreturn s".to_string())];
        let path = export_model_file(dir.path(), &files, "Edge", &dir.path().join("Edge.rbxmx")).unwrap();
        let roots = parse_place_xml(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(roots[0].children.len(), 1);
    }

    #[test]
    fn collects_luau_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/server")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/shared")).unwrap();
        std::fs::write(dir.path().join("src/server/A.server.luau"), "print(1)").unwrap();
        std::fs::write(dir.path().join("src/shared/B.luau"), "return 2").unwrap();
        std::fs::write(dir.path().join("README.md"), "# docs").unwrap();
        let files = collect_luau_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "src/server/A.server.luau");
    }

    #[test]
    fn creates_rojo_project_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_rojo_project(dir.path(), "My \"Game\"").unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "My Game");
        // Existing files are never overwritten.
        std::fs::write(&path, "{\"name\":\"custom\"}").unwrap();
        ensure_rojo_project(dir.path(), "Other").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("custom"));
    }

    #[test]
    fn rojo_build_without_rojo_is_actionable() {
        if find_rojo().is_some() {
            return; // CI machines with Rojo installed skip this branch.
        }
        let dir = tempfile::tempdir().unwrap();
        let err = rojo_build(dir.path(), &dir.path().join("out.rbxlx")).unwrap_err();
        assert!(err.to_string().contains("Rojo is not installed"));
    }
}
