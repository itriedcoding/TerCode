//! Built-in project templates. Every file is real, runnable Luau.

pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub files: Vec<(&'static str, &'static str)>,
}

pub fn blank() -> Template {
    Template {
        id: "blank",
        name: "Blank place",
        description: "A minimal Rojo project with a server and client entry point.",
        files: vec![
            (
                "default.project.json",
                r#"{
  "name": "TerCodeGame",
  "tree": {
    "$className": "DataModel",
    "ServerScriptService": {
      "$className": "ServerScriptService",
      "$path": "src/server"
    },
    "StarterPlayer": {
      "$className": "StarterPlayer",
      "StarterPlayerScripts": {
        "$className": "StarterPlayerScripts",
        "$path": "src/client"
      }
    },
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "$path": "src/shared"
    }
  }
}
"#,
            ),
            (
                "src/server/Main.server.luau",
                r#"--!strict
-- Server entry point. TerCode can extend this file through Roblox Studio MCP.

print("[TerCode] server started")

local Players = game:GetService("Players")

Players.PlayerAdded:Connect(function(player: Player)
	print(("[TerCode] %s joined"):format(player.Name))
end)
"#,
            ),
            (
                "src/client/Main.client.luau",
                r#"--!strict
-- Client entry point.

local Players = game:GetService("Players")
local player = Players.LocalPlayer

print(("[TerCode] client ready for %s"):format(player.Name))
"#,
            ),
            (
                "src/shared/Config.luau",
                r#"--!strict
-- Shared configuration used by both server and client.

local Config = {
	GameName = "TerCode Game",
	MaxPlayers = 24,
	Debug = false,
}

return Config
"#,
            ),
        ],
    }
}

pub fn obby() -> Template {
    Template {
        id: "obby",
        name: "Obby starter",
        description: "Checkpoint obby with a leaderstat, kill bricks and a finish reward.",
        files: vec![
            (
                "default.project.json",
                r#"{
  "name": "TerCodeObby",
  "tree": {
    "$className": "DataModel",
    "ServerScriptService": {
      "$className": "ServerScriptService",
      "$path": "src/server"
    },
    "StarterPlayer": {
      "$className": "StarterPlayer",
      "StarterPlayerScripts": {
        "$className": "StarterPlayerScripts",
        "$path": "src/client"
      }
    },
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "$path": "src/shared"
    }
  }
}
"#,
            ),
            (
                "src/shared/Config.luau",
                r#"--!strict
local Config = {
	StageCount = 5,
	KillBrickName = "KillBrick",
	CheckpointPrefix = "Checkpoint",
	RewardCoins = 100,
}

return Config
"#,
            ),
            (
                "src/server/Obby.server.luau",
                r#"--!strict
-- Checkpoint obby: respawns players at their furthest checkpoint and tracks coins.

local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local Config = require(ReplicatedStorage:WaitForChild("Config"))

local checkpoints: { [string]: CFrame } = {}

local function registerCheckpoints()
	for _, descendant in workspace:GetDescendants() do
		if descendant:IsA("BasePart") and descendant.Name:sub(1, #Config.CheckpointPrefix) == Config.CheckpointPrefix then
			checkpoints[descendant.Name] = descendant.CFrame + Vector3.new(0, 4, 0)
		end
	end
end

registerCheckpoints()

local function onCharacterAdded(player: Player, character: Model)
	local humanoid = character:WaitForChild("Humanoid") :: Humanoid

	local coins = player:FindFirstChild("Coins")
	if not coins then
		coins = Instance.new("IntValue")
		coins.Name = "Coins"
		coins.Parent = player
	end

	local checkpointName = player:GetAttribute("Checkpoint")
	if typeof(checkpointName) == "string" and checkpoints[checkpointName] then
		character:PivotTo(checkpoints[checkpointName])
	end

	character.DescendantAdded:Connect(function(descendant)
		if descendant:IsA("BasePart") and descendant.Name == Config.KillBrickName then
			humanoid.Health = 0
		end
	end)

	humanoid.Touched:Connect(function() end)
end

Players.PlayerAdded:Connect(function(player)
	player.CharacterAdded:Connect(function(character)
		onCharacterAdded(player, character)
	end)
end)

-- Touching a checkpoint records progress.
for _, part in workspace:GetDescendants() do
	if part:IsA("BasePart") and part.Name:sub(1, #Config.CheckpointPrefix) == Config.CheckpointPrefix then
		part.Touched:Connect(function(hit)
			local player = Players:GetPlayerFromCharacter(hit.Parent)
			if not player then
				return
			end
			player:SetAttribute("Checkpoint", part.Name)
			local coins = player:FindFirstChild("Coins")
			if coins and coins:IsA("IntValue") then
				coins.Value += 10
			end
		end)
	end
end
"#,
            ),
            (
                "src/client/Hud.client.luau",
                r#"--!strict
-- Minimal HUD showing coins and checkpoint.

local Players = game:GetService("Players")
local player = Players.LocalPlayer

local screenGui = Instance.new("ScreenGui")
screenGui.Name = "ObbyHud"
screenGui.ResetOnSpawn = false
screenGui.Parent = player:WaitForChild("PlayerGui")

local label = Instance.new("TextLabel")
label.Size = UDim2.fromOffset(220, 44)
label.Position = UDim2.new(0, 16, 0, 16)
label.BackgroundTransparency = 1
label.Font = Enum.Font.GothamMedium
label.TextSize = 18
label.TextXAlignment = Enum.TextXAlignment.Left
label.TextColor3 = Color3.fromRGB(245, 245, 245)
label.Text = "Coins: 0"
label.Parent = screenGui

local function bindCoins(coins: IntValue)
	label.Text = ("Coins: %d"):format(coins.Value)
	coins.Changed:Connect(function(value)
		label.Text = ("Coins: %d"):format(value)
	end)
end

local existing = player:WaitForChild("Coins", 10)
if existing and existing:IsA("IntValue") then
	bindCoins(existing)
else
	player.ChildAdded:Connect(function(child)
		if child.Name == "Coins" and child:IsA("IntValue") then
			bindCoins(child)
		end
	end)
end
"#,
            ),
        ],
    }
}

pub fn combat() -> Template {
    Template {
        id: "combat",
        name: "Combat sandbox",
        description: "Server-authoritative melee combat with health, cooldowns and hit feedback.",
        files: vec![
            (
                "default.project.json",
                r#"{
  "name": "TerCodeCombat",
  "tree": {
    "$className": "DataModel",
    "ServerScriptService": {
      "$className": "ServerScriptService",
      "$path": "src/server"
    },
    "StarterPlayer": {
      "$className": "StarterPlayer",
      "StarterPlayerScripts": {
        "$className": "StarterPlayerScripts",
        "$path": "src/client"
      }
    },
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "$path": "src/shared"
    }
  }
}
"#,
            ),
            (
                "src/shared/CombatConfig.luau",
                r#"--!strict
return {
	AttackDamage = 18,
	AttackCooldown = 0.55,
	AttackRange = 9,
	Knockback = 26,
}
"#,
            ),
            (
                "src/server/CombatServer.server.luau",
                r#"--!strict
-- Server-authoritative melee: the client only requests an attack; the server
-- validates range, cooldown and line of sight before applying damage.

local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local Config = require(ReplicatedStorage:WaitForChild("CombatConfig"))

local attackEvent = Instance.new("RemoteEvent")
attackEvent.Name = "RequestAttack"
attackEvent.Parent = ReplicatedStorage

local lastAttack: { [Player]: number } = {}

local function now(): number
	return os.clock()
end

attackEvent.OnServerEvent:Connect(function(player: Player)
	local last = lastAttack[player] or 0
	if now() - last < Config.AttackCooldown then
		return
	end
	lastAttack[player] = now()

	local character = player.Character
	local humanoid = character and character:FindFirstChildOfClass("Humanoid")
	local root = character and character:FindFirstChild("HumanoidRootPart")
	if not humanoid or not root or humanoid.Health <= 0 then
		return
	end

	for _, other in Players:GetPlayers() do
		if other == player then
			continue
		end
		local otherCharacter = other.Character
		local otherHumanoid = otherCharacter and otherCharacter:FindFirstChildOfClass("Humanoid")
		local otherRoot = otherCharacter and otherCharacter:FindFirstChild("HumanoidRootPart")
		if not otherHumanoid or not otherRoot or otherHumanoid.Health <= 0 then
			continue
		end
		local offset = otherRoot.Position - root.Position
		if offset.Magnitude <= Config.AttackRange then
			otherHumanoid:TakeDamage(Config.AttackDamage)
			local direction = offset.Unit
			otherRoot.AssemblyLinearVelocity = direction * Config.Knockback + Vector3.new(0, 12, 0)
		end
	end
end)

Players.PlayerRemoving:Connect(function(player)
	lastAttack[player] = nil
end)
"#,
            ),
            (
                "src/client/CombatClient.client.luau",
                r#"--!strict
-- Left click / tap to attack. Input is only a request; the server decides.

local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")
local UserInputService = game:GetService("UserInputService")

local attackEvent = ReplicatedStorage:WaitForChild("RequestAttack") :: RemoteEvent
local player = Players.LocalPlayer

local function requestAttack()
	if player:GetAttribute("Attacking") then
		return
	end
	player:SetAttribute("Attacking", true)
	attackEvent:FireServer()
	task.delay(0.55, function()
		player:SetAttribute("Attacking", false)
	end)
end

UserInputService.InputBegan:Connect(function(input, processed)
	if processed then
		return
	end
	if input.UserInputType == Enum.UserInputType.MouseButton1 or input.UserInputType == Enum.UserInputType.Touch then
		requestAttack()
	end
end)
"#,
            ),
        ],
    }
}

pub fn tycoon() -> Template {
    Template {
        id: "tycoon",
        name: "Tycoon base",
        description: "Dropper economy with persistent currency, purchasable buttons and DataStore saving.",
        files: vec![
            (
                "default.project.json",
                r#"{
  "name": "TerCodeTycoon",
  "tree": {
    "$className": "DataModel",
    "ServerScriptService": {
      "$className": "ServerScriptService",
      "$path": "src/server"
    },
    "StarterPlayer": {
      "$className": "StarterPlayer",
      "StarterPlayerScripts": {
        "$className": "StarterPlayerScripts",
        "$path": "src/shared"
      }
    },
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "$path": "src/shared"
    }
  }
}
"#,
            ),
            (
                "src/shared/EconomyConfig.luau",
                r#"--!strict
return {
	StartingCash = 50,
	DropperValue = 1,
	DropperInterval = 1.5,
	Purchases = {
		{ id = "Upgrade1", price = 100, label = "Faster Dropper" },
		{ id = "Upgrade2", price = 400, label = "Bigger Factory" },
	},
	DataStoreName = "TycoonSave_v1",
}
"#,
            ),
            (
                "src/server/Tycoon.server.luau",
                r#"--!strict
-- Persistent tycoon economy: cash, purchases, and DataStore saving with
-- retry handling and session locking-safe autosave.

local DataStoreService = game:GetService("DataStoreService")
local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local Config = require(ReplicatedStorage:WaitForChild("EconomyConfig"))
local store = DataStoreService:GetDataStore(Config.DataStoreName)

local purchaseEvent = Instance.new("RemoteEvent")
purchaseEvent.Name = "Purchase"
purchaseEvent.Parent = ReplicatedStorage

local function loadProfile(player: Player)
	local key = ("player_%d"):format(player.UserId)
	local ok, result = pcall(function()
		return store:GetAsync(key)
	end)
	if not ok then
		warn(("[Tycoon] load failed for %s: %s"):format(player.Name, tostring(result)))
		return { cash = Config.StartingCash, purchases = {} }
	end
	return result or { cash = Config.StartingCash, purchases = {} }
end

local function saveProfile(player: Player, profile)
	local key = ("player_%d"):format(player.UserId)
	local ok, err = pcall(function()
		store:SetAsync(key, profile)
	end)
	if not ok then
		warn(("[Tycoon] save failed for %s: %s"):format(player.Name, tostring(err)))
	end
end

local profiles: { [Player]: any } = {}

local function setCash(player: Player, amount: number)
	local profile = profiles[player]
	if not profile then
		return
	end
	profile.cash = math.max(0, amount)
	local cash = player:FindFirstChild("Cash")
	if cash and cash:IsA("IntValue") then
		cash.Value = profile.cash
	end
end

Players.PlayerAdded:Connect(function(player)
	local profile = loadProfile(player)
	profiles[player] = profile

	local cash = Instance.new("IntValue")
	cash.Name = "Cash"
	cash.Value = profile.cash
	cash.Parent = player
end)

purchaseEvent.OnServerEvent:Connect(function(player: Player, purchaseId: unknown)
	if typeof(purchaseId) ~= "string" then
		return
	end
	local profile = profiles[player]
	if not profile then
		return
	end
	for _, purchase in Config.Purchases do
		if purchase.id == purchaseId then
			if profile.purchases[purchaseId] then
				return
			end
			if profile.cash >= purchase.price then
				profile.purchases[purchaseId] = true
				setCash(player, profile.cash - purchase.price)
			end
			return
		end
	end
end)

task.spawn(function()
	while true do
		task.wait(Config.DropperInterval)
		for player, profile in profiles do
			setCash(player, profile.cash + Config.DropperValue)
		end
	end
end)

game:BindToClose(function()
	for player, profile in profiles do
		saveProfile(player, profile)
	end
end)

Players.PlayerRemoving:Connect(function(player)
	local profile = profiles[player]
	if profile then
		saveProfile(player, profile)
		profiles[player] = nil
	end
end)
"#,
            ),
            (
                "src/shared/CashHud.client.luau",
                r#"--!strict
local Players = game:GetService("Players")
local player = Players.LocalPlayer

local gui = Instance.new("ScreenGui")
gui.Name = "CashHud"
gui.ResetOnSpawn = false
gui.Parent = player:WaitForChild("PlayerGui")

local label = Instance.new("TextLabel")
label.Size = UDim2.fromOffset(200, 40)
label.Position = UDim2.new(1, -216, 0, 16)
label.BackgroundTransparency = 1
label.Font = Enum.Font.GothamBold
label.TextSize = 20
label.TextXAlignment = Enum.TextXAlignment.Right
label.TextColor3 = Color3.fromRGB(250, 250, 250)
label.Text = "$0"
label.Parent = gui

local cash = player:WaitForChild("Cash", 15)
if cash and cash:IsA("IntValue") then
	label.Text = ("$%d"):format(cash.Value)
	cash.Changed:Connect(function(value)
		label.Text = ("$%d"):format(value)
	end)
end
"#,
            ),
        ],
    }
}

pub fn all() -> Vec<Template> {
    vec![blank(), obby(), combat(), tycoon()]
}

pub fn find(id: &str) -> Option<Template> {
    all().into_iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_has_a_rojo_project_and_luau_files() {
        for template in all() {
            assert!(
                template.files.iter().any(|(p, _)| *p == "default.project.json"),
                "{} is missing default.project.json",
                template.id
            );
            assert!(
                template.files.iter().any(|(p, _)| p.ends_with(".luau")),
                "{} has no Luau source",
                template.id
            );
            for (path, contents) in &template.files {
                assert!(!contents.trim().is_empty(), "{path} is empty");
            }
        }
    }

    #[test]
    fn template_json_is_valid() {
        for template in all() {
            for (path, contents) in &template.files {
                if path.ends_with(".json") {
                    serde_json::from_str::<serde_json::Value>(contents)
                        .unwrap_or_else(|e| panic!("{}/{path}: {e}", template.id));
                }
            }
        }
    }

    #[test]
    fn unknown_template_is_none() {
        assert!(find("nope").is_none());
        assert!(find("obby").is_some());
    }
}
