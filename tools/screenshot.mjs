// Renders the real TerCode frontend with a stubbed Tauri bridge so the layout
// can be reviewed and captured for the README. Sample data only — it never
// touches the product code or a real Roblox session.
//
//   npm i playwright && npx playwright install chromium
//   node tools/screenshot.mjs
import { chromium } from "playwright";
import path from "node:path";
import fs from "node:fs";

const ROOT = path.resolve(new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
const UI = path.join(ROOT, "ui", "index.html");
const OUT = path.join(ROOT, "screenshots");

const now = new Date().toISOString();

const settings = {
  ai: {
    baseUrl: "https://api.terrouter.com/v1",
    model: "anthropic/claude-sonnet-4.5",
    temperature: 0.3,
    maxTokens: 8192,
    requestTimeoutSecs: 180,
    maxRetries: 3,
    customInstructions: "",
  },
  mcp: {
    autoConnect: true,
    autoReconnect: true,
    confirmDestructive: true,
    transport: "stdio",
    command: "",
    args: ["--stdio"],
    httpUrl: "http://127.0.0.1:44755/mcp",
    studioId: "",
  },
  editor: { fontSize: 13, tabSize: 4, wordWrap: false, autosaveSecs: 20 },
  ui: {
    lastProject: "p1",
    sidebarWidth: 252,
    assistantWidth: 392,
    bottomPanelHeight: 196,
    showBottomPanel: true,
  },
};

const luau = `--!strict
-- Server entry point: checkpoint obby with persistent coins.

local Players = game:GetService("Players")
local DataStoreService = game:GetService("DataStoreService")
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local Config = require(ReplicatedStorage:WaitForChild("Config"))
local store = DataStoreService:GetDataStore("Coins_v1")

local checkpoints: { [string]: CFrame } = {}

local function registerCheckpoints()
	for _, item in workspace:GetDescendants() do
		if item:IsA("BasePart") and item.Name:sub(1, #Config.CheckpointPrefix) == Config.CheckpointPrefix then
			checkpoints[item.Name] = item.CFrame + Vector3.new(0, 4, 0)
		end
	end
end

local function loadCoins(player: Player): number
	local ok, value = pcall(function()
		return store:GetAsync(tostring(player.UserId))
	end)
	if not ok then
		warn(("[Obby] load failed for %s: %s"):format(player.Name, tostring(value)))
		return 0
	end
	return value or 0
end

local function onCharacterAdded(player: Player, character: Model)
	local humanoid = character:WaitForChild("Humanoid") :: Humanoid
	local checkpoint = player:GetAttribute("Checkpoint")
	if typeof(checkpoint) == "string" and checkpoints[checkpoint] then
		character:PivotTo(checkpoints[checkpoint])
	end
	humanoid.Died:Connect(function()
		task.wait(3)
		player:LoadCharacter()
	end)
end

Players.PlayerAdded:Connect(function(player)
	registerCheckpoints()

	local coins = Instance.new("IntValue")
	coins.Name = "Coins"
	coins.Value = loadCoins(player)
	coins.Parent = player

	player.CharacterAdded:Connect(function(character)
		onCharacterAdded(player, character)
	end)
end)
`;

const files = [
  "default.project.json",
  "src/server/Obby.server.luau",
  "src/server/DataStore.server.luau",
  "src/server/Shop.server.luau",
  "src/shared/Config.luau",
  "src/client/Hud.client.luau",
  "src/client/ShopUi.client.luau",
];

function treeOf(paths) {
  const root = [];
  paths.forEach((p) => {
    const parts = p.split("/");
    let level = root;
    let acc = "";
    parts.forEach((part, index) => {
      acc = acc ? acc + "/" + part : part;
      const isDir = index < parts.length - 1;
      let node = level.find((n) => n.name === part && n.isDir === isDir);
      if (!node) {
        node = isDir
          ? { name: part, path: acc, isDir: true, size: 0, children: [] }
          : { name: part, path: acc, isDir: false, size: 2048, children: null };
        level.push(node);
      }
      if (isDir) level = node.children;
    });
  });
  const sort = (nodes) => {
    nodes.sort((a, b) => (b.isDir - a.isDir) || a.name.localeCompare(b.name));
    nodes.forEach((n) => n.children && sort(n.children));
    return nodes;
  };
  return sort(root);
}

const tools = [
  ["script_read", "Reads a script from the game using dot-notation paths, whole or by line range.", false],
  ["multi_edit", "Applies multiple edits to a script in one operation; creates it if missing.", true],
  ["search_game_tree", "Explores the instance hierarchy as a flat JSON array with filters.", false],
  ["inspect_instance", "Returns properties, attributes and children for one instance.", false],
  ["get_console_output", "Retrieves output from the Studio output log.", false],
  ["list_roblox_studios", "Lists connected Studio instances with studio and place ids.", false],
  ["save_place", "Saves the open place as a new version without publishing.", true],
  ["generate_mesh", "Generates a textured 3D mesh from a text prompt.", true],
].map(([name, description, significant]) => ({
  name,
  modelName: name,
  title: null,
  description,
  required: name === "script_read" ? ["studio_id", "path"] : ["studio_id"],
  properties: ["studio_id", "path"],
  significant,
}));

const logs = [
  ["info", "app", "TerCode 0.1.0 starting (secrets: Windows Credential Manager)"],
  ["info", "mcp", "using Roblox Studio built-in MCP (C:\\Users\\you\\AppData\\Local\\Roblox\\mcp.bat)"],
  ["info", "mcp", "connected to Roblox Studio MCP v1.4.0 (protocol 2025-06-18)"],
  ["info", "mcp", "discovered 29 tools"],
  ["info", "studio", "active Studio: studio-7c1e (Neon City, place 90210)"],
  ["info", "mcp", "→ search_game_tree"],
  ["info", "mcp", "← search_game_tree (48ms)"],
  ["info", "mcp", "→ multi_edit"],
  ["warn", "mcp", "← multi_edit (312ms)"],
  ["info", "ai", "round 2: 6 messages, 29 tools"],
  ["info", "project", "created project `Neon City` (obby)"],
].map(([level, source, message], index) => ({
  id: index + 1,
  at: now,
  level,
  source,
  message,
  detail: null,
}));

const history = [
  {
    role: "user",
    content: "Create an open-world city with roads, buildings, traffic lights and a working vehicle system in my Workspace.",
  },
  {
    role: "assistant",
    content:
      "I inspected the place first, then built the district in passes.\n\n**What now exists in Studio**\n- `Workspace.City.Roads` — 6 connected road segments on a 24-stud grid\n- `Workspace.City.Buildings` — 14 buildings with welded parts and anchored roofs\n- `Workspace.City.TrafficLights` — 4 lights driven by a shared `TrafficController` module\n- `ServerScriptService.Vehicles` — server-authoritative vehicle spawner\n\n```lua\n-- TrafficController: cycles lights deterministically for every client\nreturn function(lights: { Folder })\n\tlocal phase = 0\n\ttask.spawn(function()\n\t\twhile true do\n\t\t\tphase = (phase + 1) % 3\n\t\t\tfor _, light in lights do\n\t\t\t\tlight.Green.Enabled = phase == 0\n\t\t\t\tlight.Yellow.Enabled = phase == 1\n\t\t\t\tlight.Red.Enabled = phase == 2\n\t\t\tend\n\t\t\ttask.wait(6)\n\t\tend\n\tend)\nend\n```\n\nNext I can add pedestrian NPCs or a minimap — which would you like?",
  },
];

function bootstrapData() {
  return {
    version: "0.1.0",
    settings,
    apiKeyMasked: "tr_live_••••••4f21",
    secretBackend: "Windows Credential Manager",
    projects: [
      { id: "p1", name: "Neon City", description: "Open world", template: "obby", path: "C:\\games\\neon-city", createdAt: now, updatedAt: now, rojo: true },
      { id: "p2", name: "Tycoon Lab", description: "", template: "tycoon", path: "C:\\games\\tycoon-lab", createdAt: now, updatedAt: now, rojo: true },
    ],
    templates: [
      { id: "blank", name: "Blank place", description: "Minimal Rojo project with server and client entry points.", fileCount: 4 },
      { id: "obby", name: "Obby starter", description: "Checkpoints, kill bricks and a finish reward.", fileCount: 5 },
      { id: "combat", name: "Combat sandbox", description: "Server-authoritative melee with cooldowns.", fileCount: 4 },
      { id: "tycoon", name: "Tycoon base", description: "Droppers, purchases and DataStore saving.", fileCount: 5 },
    ],
    mcpStatus: {
      state: "connected",
      server: "Roblox Studio MCP",
      version: "1.4.0",
      toolCount: 29,
      transport: "stdio: cmd.exe /c C:\\Users\\you\\AppData\\Local\\Roblox\\mcp.bat",
      studioId: "studio-7c1e",
    },
    mcpTools: tools,
    models: [{ id: "anthropic/claude-sonnet-4.5" }, { id: "openai/gpt-5" }, { id: "google/gemini-2.5-pro" }],
    usage: { prompt_tokens: 18400, completion_tokens: 5200, total_tokens: 23600 },
    history,
    logs,
    promptHistory: [{ text: "Add a DataStore-backed currency system with a shop GUI.", at: now }],
    studio: {
      studioInstalled: true,
      studioRunning: true,
      studioExe: "C:\\Users\\you\\AppData\\Local\\Roblox\\Versions\\version-abc\\RobloxStudioBeta.exe",
      studioVersion: "version-abc",
      launcher: { kind: "builtInStudio", bat: "C:\\Users\\you\\AppData\\Local\\Roblox\\mcp.bat" },
      checked: [],
      notes: [],
      platform: "windows",
    },
    rojo: { installed: true, path: "C:\\Users\\you\\.cargo\\bin\\rojo.exe", version: "rojo 7.5.1" },
    dataDir: "C:\\Users\\you\\AppData\\Roaming\\TerCode",
  };
}

const bootstrap = bootstrapData();
const handlers = {
  bootstrap: () => bootstrap,
  list_projects: () => bootstrap.projects,
  project_tree: () => treeOf(files),
  read_file: ({ path: p }) => (p.includes("Obby") ? luau : luau.replace(/Obby/g, "Module")),
  analyze_file: () => [
    { line: 24, column: 1, severity: "warning", code: "deprecated-api", message: "use task.wait instead of the deprecated wait()" },
    { line: 3, column: 1, severity: "info", code: "no-strict", message: "add `--!strict` for Luau type checking in Studio" },
  ],
  mcp_tools: () => tools,
  studio_discover: () => bootstrap.studio,
  prompt_history: () => bootstrap.promptHistory,
  get_logs: () => logs,
  list_models: () => bootstrap.models,
  mcp_status: () => bootstrap.mcpStatus,
  update_settings: () => settings,
  app_info: () => ({ version: "0.1.0", platform: "windows", arch: "x86_64", dataDir: bootstrap.dataDir }),
};

async function main() {
  fs.mkdirSync(OUT, { recursive: true });
  // Set PLAYWRIGHT_CHROMIUM_PATH when the bundled headless shell is unavailable.
  const browser = await chromium.launch({
    executablePath: process.env.PLAYWRIGHT_CHROMIUM_PATH || undefined,
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"],
  });
  const page = await browser.newPage({
    viewport: { width: 1480, height: 940 },
    deviceScaleFactor: 2,
    colorScheme: "dark",
  });

  await page.addInitScript(
    (payload) => {
      window.__stubHandlers = payload;
      window.__TAURI__ = {
        core: {
          invoke: (command, args) =>
            Promise.resolve(window.__stubHandlers[command] ? window.__stubHandlers[command](args || {}) : null),
        },
        event: { listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() },
      };
    },
    handlers
  );

  const problems = [];
  page.on("pageerror", (error) => problems.push("pageerror: " + error.message));
  page.on("console", (message) => {
    if (message.type() === "error") problems.push("console: " + message.text());
  });

  await page.goto("file://" + UI);
  await page.waitForTimeout(900);
  await page.screenshot({ path: path.join(OUT, "01-workspace.png") });

  await page.click('[data-view="projects"]');
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(OUT, "02-projects.png") });

  await page.click('[data-view="tools"]');
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(OUT, "03-tools.png") });

  await page.click("#openSettings");
  await page.waitForTimeout(350);
  await page.click('[data-stab="mcp"]');
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(OUT, "04-settings-mcp.png") });
  await page.keyboard.press("Escape");

  await page.click('[data-view="explorer"]');
  await page.click('[data-panel="activity"]');
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(OUT, "05-activity.png") });

  await browser.close();
  console.log("screenshots written to", OUT);
  if (problems.length) {
    console.log("UI problems detected:");
    problems.forEach((problem) => console.log("  " + problem));
    process.exitCode = 3;
  } else {
    console.log("no console or page errors");
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
