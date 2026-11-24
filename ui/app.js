/* TerCode — desktop frontend.
   Talks to the Rust core exclusively through Tauri commands and events. */
(function () {
  "use strict";

  const tauri = window.__TAURI__;
  const invoke = tauri && tauri.core ? tauri.core.invoke : null;
  const listen = tauri && tauri.event ? tauri.event.listen : null;

  const $ = (id) => document.getElementById(id);
  const el = (tag, cls, html) => {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (html !== undefined) node.innerHTML = html;
    return node;
  };

  const S = {
    boot: null,
    settings: null,
    projects: [],
    projectId: null,
    tree: [],
    files: [],
    tabs: [],
    activePath: null,
    dirty: false,
    models: [],
    tools: [],
    mcpStatus: { state: "disconnected" },
    logs: [],
    activity: [],
    studioReport: null,
    view: "projects",
    messages: [],
    streamText: "",
    streamEl: null,
    streamQueue: "",
    agentBusy: false,
    usage: { total_tokens: 0 },
    diagnostics: [],
    bottomPanel: "output",
    searchHits: null,
    templates: [],
    promptHistory: [],
    pendingConfirm: null,
    editor: null,
    modelFilter: "",
    attachActive: true,
    screenshots: [],
    settingsDraft: null,
    projectReview: null,
    selfTest: {
      checks: [],
      running: false,
      verdict: null,
      summary: "",
      streamEl: null,
      streamText: "",
      streamQueue: "",
    },
  };

  let started = false;

  // ---------------------------------------------------------------- utilities

  function toast(message, kind) {
    const node = el("div", "toast " + (kind || ""));
    const icon = kind === "error" ? "i-alert" : kind === "ok" ? "i-check" : "i-sparkle";
    node.innerHTML = '<svg class="ic"><use href="#' + icon + '"/></svg><span></span>';
    node.querySelector("span").textContent = message;
    $("toasts").appendChild(node);
    setTimeout(() => {
      node.style.transition = "opacity 200ms, transform 200ms";
      node.style.opacity = "0";
      node.style.transform = "translateY(6px)";
      setTimeout(() => node.remove(), 220);
    }, kind === "error" ? 6000 : 3600);
  }

  function shortPath(path) {
    if (!path) return "—";
    const parts = path.split("/");
    return parts.slice(-2).join("/");
  }

  function timeOf(iso) {
    try {
      return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    } catch (e) {
      return "";
    }
  }

  function relTime(iso) {
    try {
      const diff = (Date.now() - new Date(iso).getTime()) / 1000;
      if (diff < 60) return "just now";
      if (diff < 3600) return Math.floor(diff / 60) + "m ago";
      if (diff < 86400) return Math.floor(diff / 3600) + "h ago";
      return Math.floor(diff / 86400) + "d ago";
    } catch (e) {
      return "";
    }
  }

  function errorText(error) {
    if (!error) return "Unknown error";
    if (typeof error === "string") return error;
    return error.message || JSON.stringify(error);
  }

  // ------------------------------------------------------------------ backend

  async function call(command, args) {
    if (!invoke) throw new Error("TerCode backend is not available (open the desktop app).");
    return invoke(command, args || {});
  }

  async function safeCall(command, args, options) {
    try {
      return await call(command, args);
    } catch (error) {
      const message = errorText(error);
      if (!options || !options.quiet) toast(message, "error");
      throw error;
    }
  }

  // ------------------------------------------------------------------ bootstrap

  async function start() {
    // index.html loads this file at the end of <body>, so readyState is no
    // longer "loading" and start() would otherwise run twice (once from the
    // readyState check and once on DOMContentLoaded), doubling every listener.
    if (started) return;
    started = true;
    bindChrome();
    bindMessages();
    bindShortcuts();
    setupEditor();

    if (!invoke) {
      $("welcomeStatus").textContent =
        "The desktop backend is not connected. Launch the TerCode app (cargo tauri dev) to use Studio integration.";
      toast("Running without the desktop backend", "error");
      return;
    }

    try {
      const boot = await call("bootstrap");
      applyBootstrap(boot);
      await attachListeners();
      await refreshStudio();
      if (boot.settings.ui.lastProject) {
        const exists = boot.projects.find((p) => p.id === boot.settings.ui.lastProject);
        if (exists) await openProject(exists.id);
      }
      if (!boot.apiKeyMasked) openSettings("ai");
    } catch (error) {
      toast(errorText(error), "error");
      $("welcomeStatus").textContent = errorText(error);
    }

    setInterval(autosaveTick, Math.max(5, (S.settings ? S.settings.editor.autosaveSecs : 20)) * 1000);
    setInterval(refreshStudio, 15000);
  }

  function applyBootstrap(boot) {
    S.boot = boot;
    S.settings = boot.settings;
    S.projects = boot.projects;
    S.templates = boot.templates;
    S.models = boot.models || [];
    S.tools = boot.mcpTools || [];
    S.logs = boot.logs || [];
    S.promptHistory = boot.promptHistory || [];
    S.mcpStatus = boot.mcpStatus;
    S.usage = boot.usage || { total_tokens: 0 };
    S.studioReport = boot.studio;

    document.documentElement.style.setProperty("--sidebar-w", boot.settings.ui.sidebarWidth + "px");
    document.documentElement.style.setProperty("--assistant-w", boot.settings.ui.assistantWidth + "px");
    $("bottomPanel").style.setProperty("--panel-h", boot.settings.ui.bottomPanelHeight + "px");
    if (!boot.settings.ui.showBottomPanel) $("bottomPanel").classList.add("collapsed");

    renderModelLabel();
    renderProjects();
    renderSidebar();
    renderTemplatesBar();
    renderStatus();
    renderLogs();
    renderStudioPanel();
    renderMessagesFromHistory();

    if (boot.settings.ai.model) {
      $("assistantModel").textContent = shortModel(boot.settings.ai.model);
    }
    applyEditorPrefs();
    updateAutoApproveChip();
    renderSelfTest();
    updateUsage();
    $("welcomeStatus").innerHTML =
      "Studio: <strong>" + (boot.studio.studioInstalled ? "installed" : "not found") + "</strong> · " +
      "MCP: <strong>" + statusLabel(S.mcpStatus) + "</strong> · " +
      "Models: <strong>" + (S.models.length || 0) + "</strong> · v" + boot.version;
  }

  async function attachListeners() {
    if (!listen) return;
    await listen("tercode://log", (event) => {
      S.logs.push(event.payload);
      if (S.logs.length > 800) S.logs.shift();
      appendLogLine(event.payload);
    });
    await listen("tercode://mcp", (event) => {
      const payload = event.payload;
      if (payload.type === "status") {
        S.mcpStatus = payload.status;
        renderStatus();
        renderStudioPanel();
        if (payload.status.state === "connected") {
          call("mcp_tools").then((tools) => {
            S.tools = tools;
            renderTools();
          }).catch(() => {});
        }
      } else if (payload.type === "tools") {
        S.tools = payload.tools;
        renderTools();
      } else if (payload.type === "activity") {
        S.activity.push(payload);
        if (S.activity.length > 400) S.activity.shift();
        if (S.bottomPanel === "activity") renderActivityPanel();
      }
    });
    // Runs are tagged with a source: the chat ignores self-test output and the
    // self-test page ignores chat output, so both can stream into one channel.
    await listen("tercode://agent", (event) => routeAgentEvent(event.payload));
    await listen("tercode://agent-done", (event) => {
      if (envelopeSource(event.payload) === "chat") finishAgent();
    });
    await listen("tercode://agent-error", (event) => {
      if (envelopeSource(event.payload) !== "chat") return;
      finishAgent();
      const error = envelopeBody(event.payload);
      appendMessage("assistant", "**Request failed.** " + errorText(error));
      toast(errorText(error), "error");
    });
    await listen("tercode://self-test", (event) => handleSelfTestEvent(event.payload));
    await listen("tercode://confirm", (event) => renderConfirmation(event.payload));
  }

  function envelopeSource(payload) {
    return payload && payload.source ? payload.source : "chat";
  }

  function envelopeBody(payload) {
    return payload && payload.event !== undefined ? payload.event : payload;
  }

  function routeAgentEvent(payload) {
    const source = envelopeSource(payload);
    const body = envelopeBody(payload);
    if (source === "self-test") handleSelfTestEvent(body);
    else handleAgentEvent(body);
  }

  // -------------------------------------------------------------------- chrome

  function bindChrome() {
    document.querySelectorAll(".rail-btn").forEach((button) => {
      button.addEventListener("click", () => {
        const view = button.dataset.view;
        if (view === "settings") {
          openSettings();
          return;
        }
        if (view === "selftest") {
          openSelfTest();
          return;
        }
        setView(view);
      });
    });

    $("openSettings").addEventListener("click", () => openSettings());
    $("commandBtn").addEventListener("click", openPalette);
    $("scanProject").addEventListener("click", () => scanProject({ openPanel: true }));
    $("testScan").addEventListener("click", scanSelfTestChecks);
    $("runSelfTest").addEventListener("click", runSelfTest);
    $("welcomeTest").addEventListener("click", () => {
      openSelfTest();
      runSelfTest();
    });
    $("runTestBtn").addEventListener("click", () => {
      openSelfTest();
      runSelfTest();
    });
    $("sponsorLink").addEventListener("click", (event) => {
      event.preventDefault();
      call("open_external", { url: "https://terrouter.com" }).catch(() => {});
    });

    $("sideAction").addEventListener("click", () => {
      if (S.view === "projects") newProjectModal();
      else if (S.view === "templates") setView("templates");
      else if (S.view === "explorer") newFileModal();
      else if (S.view === "tools") call("mcp_refresh_tools").then((tools) => {
        S.tools = tools;
        renderTools();
        toast("Tool list refreshed", "ok");
      }).catch(() => {});
    });

    let searchTimer = null;
    $("sideSearch").addEventListener("input", (event) => {
      const query = event.target.value;
      clearTimeout(searchTimer);
      if (!S.projectId) return;
      searchTimer = setTimeout(async () => {
        if (!query.trim()) {
          S.searchHits = null;
          renderSidebar();
          return;
        }
        try {
          S.searchHits = await call("search_project", { id: S.projectId, query });
          renderSidebar();
        } catch (error) {
          S.searchHits = null;
        }
      }, 220);
    });

    $("projectChip").addEventListener("click", () => setView("projects"));
    $("modelButton").addEventListener("click", (event) => {
      event.stopPropagation();
      toggleModelMenu($("modelButton"));
    });
    $("assistantModel").addEventListener("click", (event) => {
      event.stopPropagation();
      toggleModelMenu($("assistantModel"));
    });
    document.addEventListener("click", (event) => {
      const popover = $("modelPopover");
      if (popover && !popover.contains(event.target) && !event.target.closest("#modelButton, #assistantModel")) {
        closeModelMenu();
      }
    });

    $("studioPill").addEventListener("click", () => {
      S.bottomPanel = "studio";
      $("bottomPanel").classList.remove("collapsed");
      setBottomTab("studio");
      renderStudioPanel();
    });
    $("welcomeNew").addEventListener("click", newProjectModal);
    $("welcomeTemplate").addEventListener("click", () => setView("templates"));

    $("clearOutput").addEventListener("click", async () => {
      await call("clear_logs").catch(() => {});
      S.logs = [];
      renderLogs();
    });
    $("toggleBottom").addEventListener("click", toggleBottomPanel);
    $("saveFile").addEventListener("click", () => saveActiveFile());
    $("askAboutFile").addEventListener("click", () => {
      if (!S.activePath) return;
      const prompt = "Review `" + S.activePath + "`, explain the issues you find and propose a concrete fix. Apply the fix if it is safe.";
      $("promptInput").value = prompt;
      sendPrompt();
    });
    $("formatFile").addEventListener("click", () => analyseActive(true));
  }

  // ---------------------------------------------------------------- navigation

  function setView(view) {
    S.view = view;
    document.querySelectorAll(".rail-btn").forEach((button) => {
      button.classList.toggle("is-active", button.dataset.view === view);
    });
    const titles = { projects: "Projects", explorer: "Explorer", templates: "Templates", tools: "Studio MCP", settings: "Settings", selftest: "Self-test" };
    $("sideTitle").textContent = titles[view] || "Workspace";
    $("sideAction").title =
      view === "projects" ? "New project" : view === "explorer" ? "New file" : "Refresh tools";
    $("sideAction").hidden = view === "settings" || view === "templates" || view === "selftest";
    $("sideSearch").closest(".input-row").style.display =
      view === "explorer" || view === "projects" ? "" : "none";
    $("settingsView").hidden = view !== "settings";
    $("testView").hidden = view !== "selftest";
    $("editorHost").hidden = view === "settings" || view === "selftest";
    $("tabs").hidden = view === "settings" || view === "selftest";
    document.querySelector(".main").classList.toggle("focus-mode", view === "selftest");
    if (view !== "settings" && view !== "selftest" && S.settings && S.settings.ui) {
      $("bottomPanel").classList.toggle("collapsed", !S.settings.ui.showBottomPanel);
    }
    if (view === "selftest") {
      $("bottomPanel").classList.add("collapsed");
      renderSelfTest();
    }
    renderSidebar();
  }

  function openSelfTest() {
    if (S.view === "settings") commitSettingsDraft();
    setView("selftest");
    if (S.projectId && !S.selfTest.running && !S.selfTest.checks.length) scanSelfTestChecks();
  }

  function renderSidebar() {
    const body = $("sideBody");
    body.innerHTML = "";
    $("sideFoot").hidden = true;
    if (S.view === "projects") return renderProjectList(body);
    if (S.view === "explorer") return renderExplorer(body);
    if (S.view === "templates") return renderTemplateList(body);
    if (S.view === "tools") return renderToolList(body);
    if (S.view === "settings") return renderSettingsNav(body);
  }

  function renderSettingsNav(container) {
    [
      ["ai", "AI Provider", "Ter Router key, model and limits"],
      ["mcp", "Roblox MCP", "Transport, connection and approvals"],
      ["testing", "Verification", "AI self-test, playtest and console"],
      ["editor", "Editor", "Font, tabs and autosave"],
      ["about", "About", "Version, folders and updates"],
    ].forEach(([id, name, hint]) => {
      const row = el("button", "tool-row");
      row.innerHTML = '<span class="tr-name"></span><span class="tr-desc"></span>';
      row.querySelector(".tr-name").textContent = name;
      row.querySelector(".tr-desc").textContent = hint;
      row.addEventListener("click", () => openSettings(id));
      container.appendChild(row);
    });
  }

  // Event handlers refresh the discovered tool list in place.
  function renderTools() {
    if (S.view === "tools") renderSidebar();
  }

  function renderProjectList(container) {
    if (!S.projects.length) {
      container.appendChild(el("div", "side-empty",
        "No projects yet.<br/>Create one to start building, or import an existing Rojo folder."));
      const row = el("div", "");
      const create = el("button", "btn primary", '<svg class="ic"><use href="#i-plus"/></svg>New project');
      create.addEventListener("click", newProjectModal);
      const importBtn = el("button", "btn", '<svg class="ic"><use href="#i-folder"/></svg>Import');
      importBtn.style.marginLeft = "6px";
      importBtn.addEventListener("click", importProject);
      row.appendChild(create);
      row.appendChild(importBtn);
      container.appendChild(row);
      return;
    }
    S.projects.forEach((project) => {
      const card = el("button", "project-card" + (project.id === S.projectId ? " is-active" : ""));
      card.innerHTML =
        '<span class="pc-name"></span>' +
        '<span class="pc-meta"><span>' + (project.rojo ? "Rojo" : project.template) + '</span>' +
        '<span class="dot">•</span><span>' + relTime(project.updatedAt) + "</span></span>";
      card.querySelector(".pc-name").textContent = project.name;
      card.addEventListener("click", () => openProject(project.id));
      container.appendChild(card);
    });
  }

  function renderExplorer(container) {
    if (S.searchHits) {
      if (!S.searchHits.length) {
        container.appendChild(el("div", "side-empty", "No matches in this project."));
        return;
      }
      S.searchHits.forEach((hit) => {
        const row = el("button", "search-hit");
        row.innerHTML = '<div class="sh-path"></div><div class="sh-line"></div>';
        row.querySelector(".sh-path").textContent = hit.path + ":" + hit.line;
        row.querySelector(".sh-line").textContent = hit.text;
        row.addEventListener("click", () => openFile(hit.path, hit.line));
        container.appendChild(row);
      });
      return;
    }
    if (!S.projectId) {
      container.appendChild(el("div", "side-empty", "Open a project to browse its files."));
      return;
    }
    if (!S.tree.length) {
      container.appendChild(el("div", "side-empty", "This project is empty. Create a file to begin."));
    }
    container.appendChild(buildTree(S.tree, 0));

    const foot = $("sideFoot");
    foot.hidden = false;
    foot.innerHTML = "";
    const actions = [
      ["New file", newFileModal],
      ["Backup", backupProject],
      ["Versions", versionsModal],
      ["Export .rbxmx", exportModel],
      ["Rojo build", rojoBuild],
    ];
    actions.forEach(([label, handler]) => {
      const button = el("button", "btn ghost", label);
      button.style.width = "100%";
      button.style.justifyContent = "flex-start";
      button.style.fontSize = "11.5px";
      button.addEventListener("click", handler);
      foot.appendChild(button);
    });
  }

  function buildTree(nodes, depth) {
    const wrap = el("div", "tree");
    nodes.forEach((node) => {
      const row = el("div", "tree-row");
      if (node.isDir) {
        row.innerHTML =
          '<svg class="ic caret' + (node._open ? " open" : "") + '"><use href="#i-chevron"/></svg>' +
          '<svg class="ic"><use href="#i-folder"/></svg><span class="fname"></span>';
        row.querySelector(".fname").textContent = node.name;
        row.style.paddingLeft = 6 + depth * 12 + "px";
        row.addEventListener("click", () => {
          node._open = !node._open;
          renderSidebar();
        });
        wrap.appendChild(row);
        if (node._open && node.children) wrap.appendChild(buildTree(node.children, depth + 1));
      } else {
        row.innerHTML = '<span style="width:10px"></span><svg class="ic"><use href="#i-file"/></svg><span class="fname"></span>';
        row.querySelector(".fname").textContent = node.name;
        row.style.paddingLeft = 6 + depth * 12 + "px";
        row.classList.toggle("is-active", node.path === S.activePath);
        row.addEventListener("click", () => openFile(node.path));
        row.addEventListener("contextmenu", (event) => {
          event.preventDefault();
          fileMenu(node.path);
        });
        wrap.appendChild(row);
      }
    });
    return wrap;
  }

  function fileMenu(path) {
    openModal(
      "<h2>File actions</h2><p class='sub'>" + MD.escape(path) + "</p>",
      {
        actions: [
          { label: "Rename", handler: () => { closeModal(); renameFileModal(path); } },
          { label: "Delete", kind: "danger", handler: () => { closeModal(); deleteFile(path); } },
          { label: "Close", kind: "ghost", handler: closeModal },
        ],
      }
    );
  }

  function renderTemplateList(container) {
    S.templates.forEach((template) => {
      const card = el("div", "template-card");
      card.innerHTML = '<div class="tc-name"></div><div class="tc-desc"></div>';
      card.querySelector(".tc-name").textContent = template.name;
      card.querySelector(".tc-desc").textContent = template.description + " · " + template.fileCount + " files";
      card.addEventListener("click", () => newProjectModal(template.id));
      container.appendChild(card);
    });
    if (!S.templates.length) container.appendChild(el("div", "side-empty", "No templates available."));
  }

  function renderToolList(container) {
    if (!S.tools.length) {
      container.appendChild(el("div", "side-empty",
        "No MCP tools discovered yet.<br/>Start Roblox Studio with the MCP server enabled, then reconnect."));
      return;
    }
    S.tools.forEach((tool) => {
      const row = el("button", "tool-row");
      row.innerHTML =
        '<span class="tr-name"><span></span>' +
        (tool.significant ? '<span class="tag">writes</span>' : "") +
        '</span><span class="tr-desc"></span>';
      row.querySelector(".tr-name span").textContent = tool.name;
      row.querySelector(".tr-desc").textContent = tool.description || tool.title || "No description";
      row.addEventListener("click", () => toolModal(tool));
      container.appendChild(row);
    });
  }

  // ------------------------------------------------------------------ projects

  async function newProjectModal(templateId) {
    const options = S.templates
      .map((t) => '<option value="' + t.id + '"' + (t.id === (templateId || "blank") ? " selected" : "") + ">" + MD.escape(t.name) + "</option>")
      .join("");
    openModal(
      "<h2>New project</h2><p class='sub'>Creates a Rojo-ready project on disk with real Luau sources.</p>" +
      "<div class='field'><label>Project name</label><input type='text' id='npName' placeholder='Neon City' autocomplete='off'></div>" +
      "<div class='field'><label>Template</label><select id='npTemplate'>" + options + "</select></div>" +
      "<div class='field'><label>Description (optional)</label><input type='text' id='npDesc' placeholder='Open-world city with traffic'></div>",
      {
        actions: [
          { label: "Cancel", kind: "ghost", handler: closeModal },
          {
            label: "Create project",
            kind: "primary",
            handler: async () => {
              const name = $("npName").value.trim();
              if (!name) { toast("Give the project a name", "error"); return; }
              const project = await safeCall("create_project", {
                name,
                template: $("npTemplate").value,
                description: $("npDesc").value,
              });
              closeModal();
              S.projects = await call("list_projects");
              renderProjects();
              await openProject(project.id);
              toast("Project created", "ok");
            },
          },
        ],
      }
    );
    setTimeout(() => $("npName").focus(), 60);
  }

  async function importProject() {
    let path = null;
    try {
      path = await call("pick_folder");
    } catch (error) {
      path = null;
    }
    if (!path) {
      openModal(
        "<h2>Import project folder</h2><p class='sub'>Point TerCode at a folder containing a Rojo project (default.project.json).</p>" +
        "<div class='field'><label>Folder path</label><input type='text' id='ipPath' placeholder='C:\\dev\\my-game'></div>",
        {
          actions: [
            { label: "Cancel", kind: "ghost", handler: closeModal },
            {
              label: "Import",
              kind: "primary",
              handler: async () => {
                const typed = $("ipPath").value.trim();
                if (!typed) return;
                const project = await safeCall("import_project", { path: typed });
                closeModal();
                S.projects = await call("list_projects");
                renderProjects();
                await openProject(project.id);
              },
            },
          ],
        }
      );
      return;
    }
    const project = await safeCall("import_project", { path });
    S.projects = await call("list_projects");
    renderProjects();
    await openProject(project.id);
    toast("Imported " + project.name, "ok");
  }

  async function openProject(id) {
    if (S.dirty) await saveActiveFile(true);
    S.projectId = id;
    S.searchHits = null;
    S.tabs = [];
    S.activePath = null;
    S.dirty = false;
    S.diagnostics = [];
    S.projectReview = null;
    if (!S.selfTest.running) {
      S.selfTest.checks = [];
      S.selfTest.verdict = null;
      S.selfTest.summary = "";
      clearTestLog();
      renderSelfTest();
    }
    renderTabs();
    $("editor").hidden = true;
    $("welcome").hidden = false;
    const project = S.projects.find((p) => p.id === id);
    $("projectChipName").textContent = project ? project.name : "No project";
    $("ctxProject").textContent = project ? project.name : "No project";
    renderProjects();
    await refreshTree();
    const settings = { ...S.settings };
    settings.ui = { ...settings.ui, lastProject: id };
    S.settings = settings;
    call("update_settings", { settings }).catch(() => {});
    setView("explorer");
    if (S.files.length) {
      const preferred = S.files.find((f) => f.endsWith("Main.server.luau")) || S.files[0];
      await openFile(preferred);
    }
  }

  async function refreshTree() {
    if (!S.projectId) return;
    try {
      S.tree = await call("project_tree", { id: S.projectId });
      S.files = [];
      (function walk(nodes) {
        nodes.forEach((node) => {
          if (node.isDir) { if (node.children) walk(node.children); }
          else S.files.push(node.path);
        });
      })(S.tree);
      S.tree.forEach((node) => { if (node.isDir) node._open = true; });
    } catch (error) {
      S.tree = [];
    }
    if (S.view === "explorer") renderSidebar();
  }

  function renderProjects() {
    if (S.view === "projects") renderSidebar();
    const current = S.projects.find((p) => p.id === S.projectId);
    $("projectChipName").textContent = current ? current.name : "No project";
  }

  // --------------------------------------------------------------------- files

  async function openFile(path, line) {
    if (!S.projectId) return;
    if (S.view === "settings") setView("explorer");
    if (S.dirty && S.activePath && S.activePath !== path) await saveActiveFile(true);
    let content = "";
    try {
      content = await call("read_file", { id: S.projectId, path });
    } catch (error) {
      toast(errorText(error), "error");
      return;
    }
    S.activePath = path;
    S.dirty = false;
    if (!S.tabs.find((tab) => tab.path === path)) S.tabs.push({ path, dirty: false });
    S.tabs.forEach((tab) => { tab.dirty = tab.path === path ? false : tab.dirty; });
    renderTabs();
    $("welcome").hidden = true;
    $("editor").hidden = false;
    $("filePath").textContent = path;
    S.editor.setPath(path);
    S.editor.setContent(content, path);
    S.editor.focus();
    if (line) {
      const textarea = $("code");
      const lines = content.split("\n");
      let offset = 0;
      for (let i = 0; i < Math.min(line - 1, lines.length); i += 1) offset += lines[i].length + 1;
      textarea.focus();
      textarea.setSelectionRange(offset, offset + (lines[line - 1] || "").length);
    }
    await analyseActive(false);
    renderSidebar();
  }

  function renderTabs() {
    const tabs = $("tabs");
    tabs.innerHTML = "";
    S.tabs.forEach((tab) => {
      const node = el("div", "tab" + (tab.path === S.activePath ? " is-active" : ""));
      node.innerHTML =
        '<span class="tname"></span>' +
        (tab.dirty ? '<span class="dirty"></span>' : "") +
        '<svg class="ic close" style="width:12px;height:12px"><use href="#i-x"/></svg>';
      node.querySelector(".tname").textContent = tab.path.split("/").pop();
      node.title = tab.path;
      node.addEventListener("click", (event) => {
        if (event.target.classList.contains("close")) {
          closeTab(tab.path);
        } else if (tab.path !== S.activePath) {
          openFile(tab.path);
        }
      });
      tabs.appendChild(node);
    });
  }

  async function closeTab(path) {
    if (path === S.activePath && S.dirty) await saveActiveFile(true);
    S.tabs = S.tabs.filter((tab) => tab.path !== path);
    if (S.activePath === path) {
      S.activePath = null;
      $("editor").hidden = true;
      $("welcome").hidden = false;
      S.diagnostics = [];
      renderDiagnostics();
      if (S.tabs.length) await openFile(S.tabs[S.tabs.length - 1].path);
    }
    renderTabs();
  }

  async function saveActiveFile(silent) {
    if (!S.projectId || !S.activePath) return;
    const content = S.editor.getContent();
    setSaveState("saving", "Saving…");
    try {
      await call("write_file", { id: S.projectId, path: S.activePath, contents: content });
      S.dirty = false;
      S.tabs.forEach((tab) => { if (tab.path === S.activePath) tab.dirty = false; });
      renderTabs();
      setSaveState("", "Saved " + new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }));
      if (!silent) toast("Saved " + shortPath(S.activePath), "ok");
    } catch (error) {
      setSaveState("dirty", "Save failed");
      toast(errorText(error), "error");
    }
  }

  function setSaveState(kind, label) {
    const node = $("saveState");
    node.className = "save-state " + (kind || "");
    node.textContent = label;
  }

  let analyseTimer = null;
  async function analyseActive(verbose) {
    if (!S.activePath) return;
    try {
      S.diagnostics = await call("analyze_file", { path: S.activePath, contents: S.editor.getContent() });
    } catch (error) {
      S.diagnostics = [];
    }
    S.editor.markDiagnostics(S.diagnostics);
    renderDiagnostics();
    if (S.bottomPanel === "diagnostics") renderDiagnosticsPanel();
    if (verbose) {
      const errors = S.diagnostics.filter((d) => d.severity === "error").length;
      const warnings = S.diagnostics.filter((d) => d.severity === "warning").length;
      toast(errors + " error(s), " + warnings + " warning(s)", errors ? "error" : "ok");
    }
  }

  function renderDiagnostics() {
    const strip = $("diagStrip");
    const errors = S.diagnostics.filter((d) => d.severity === "error");
    const warnings = S.diagnostics.filter((d) => d.severity === "warning");
    const projectErrors = S.projectReview ? S.projectReview.errors : 0;
    $("diagCount").textContent = String(S.projectReview ? projectErrors : S.diagnostics.length);
    $("diagCount").classList.toggle("has", errors.length > 0 || projectErrors > 0);
    if (!S.diagnostics.length) {
      strip.hidden = true;
      return;
    }
    strip.hidden = false;
    strip.innerHTML = "";
    S.diagnostics.slice(0, 8).forEach((diagnostic) => {
      const cls = diagnostic.severity === "error" ? "err" : diagnostic.severity === "warning" ? "warn" : "info";
      const chip = el("span", "d " + cls);
      chip.textContent = "L" + diagnostic.line + " · " + diagnostic.message;
      chip.addEventListener("click", () => openFile(S.activePath, diagnostic.line));
      strip.appendChild(chip);
    });
    if (S.diagnostics.length > 8) {
      strip.appendChild(el("span", "d info", "+" + (S.diagnostics.length - 8) + " more"));
    }
  }

  // ---------------------------------------------------------------- file modals

  function newFileModal() {
    if (!S.projectId) { toast("Open a project first", "error"); return; }
    openModal(
      "<h2>New file</h2><p class='sub'>Create a Luau script or JSON config inside the project.</p>" +
      "<div class='field'><label>Path</label><input type='text' id='nfPath' value='src/server/' autocomplete='off'></div>" +
      "<div class='field'><label>Kind</label><select id='nfKind'>" +
      "<option value='.server.luau'>Server Script</option>" +
      "<option value='.client.luau'>LocalScript</option>" +
      "<option value='.luau'>ModuleScript</option>" +
      "<option value='.json'>JSON</option>" +
      "<option value=''>Exact path</option></select></div>" +
      "<div class='field'><label>Name</label><input type='text' id='nfName' placeholder='GameLoop' autocomplete='off'></div>",
      {
        actions: [
          { label: "Cancel", kind: "ghost", handler: closeModal },
          {
            label: "Create",
            kind: "primary",
            handler: async () => {
              const base = $("nfPath").value.trim().replace(/\/$/, "");
              const name = $("nfName").value.trim();
              const kind = $("nfKind").value;
              if (!name) { toast("Name the file", "error"); return; }
              const path = kind ? base + "/" + name + kind : name;
              await safeCall("create_file", { id: S.projectId, path });
              closeModal();
              await refreshTree();
              await openFile(path.replace(/^\/+/, ""));
            },
          },
        ],
      }
    );
    setTimeout(() => $("nfName").focus(), 60);
  }

  function renameFileModal(path) {
    openModal(
      "<h2>Rename file</h2><p class='sub'>" + MD.escape(path) + "</p>" +
      "<div class='field'><label>New path</label><input type='text' id='rnPath' autocomplete='off'></div>",
      {
        actions: [
          { label: "Cancel", kind: "ghost", handler: closeModal },
          {
            label: "Rename",
            kind: "primary",
            handler: async () => {
              const next = $("rnPath").value.trim();
              if (!next) return;
              await safeCall("rename_path", { id: S.projectId, from: path, to: next });
              closeModal();
              S.tabs = S.tabs.filter((tab) => tab.path !== path);
              if (S.activePath === path) S.activePath = null;
              renderTabs();
              await refreshTree();
            },
          },
        ],
      }
    );
    setTimeout(() => { $("rnPath").value = path; $("rnPath").focus(); $("rnPath").select(); }, 60);
  }

  async function deleteFile(path) {
    if (!window.confirm("Delete " + path + "? A version snapshot is kept in .tercode/versions.")) return;
    await safeCall("delete_path", { id: S.projectId, path });
    S.tabs = S.tabs.filter((tab) => tab.path !== path);
    if (S.activePath === path) {
      S.activePath = null;
      $("editor").hidden = true;
      $("welcome").hidden = false;
    }
    renderTabs();
    await refreshTree();
  }

  async function versionsModal() {
    if (!S.projectId) return;
    const versions = await safeCall("list_versions", { id: S.projectId });
    const backups = await safeCall("list_backups", { id: S.projectId });
    const versionRows = versions.length
      ? versions.map((v) =>
          "<div class='prompt-history-item' data-version='" + v.id + "'><span>" + MD.escape(v.path) +
          "</span><span class='when'>" + relTime(v.at) + " · " + Math.round(v.bytes / 1024) + " KB</span></div>").join("")
      : "<p class='sub'>No file snapshots yet — they are captured on every save.</p>";
    const backupRows = backups.length
      ? backups.map((b) =>
          "<div class='prompt-history-item' data-backup='" + b.id + "'><span>Backup " + MD.escape(b.id) +
          "</span><span class='when'>" + Math.round(b.bytes / 1024) + " KB</span></div>").join("")
      : "<p class='sub'>No full backups yet.</p>";
    openModal(
      "<h2>Version history</h2><p class='sub'>Snapshots restore the previous contents of a file.</p>" +
      "<div class='field'><label>File snapshots</label>" + versionRows + "</div>" +
      "<div class='field'><label>Full project backups</label>" + backupRows + "</div>",
      {
        actions: [
          { label: "Backup now", handler: async () => {
              await safeCall("backup_project", { id: S.projectId, label: "manual" });
              closeModal();
              toast("Backup created", "ok");
            } },
          { label: "Close", kind: "primary", handler: closeModal },
        ],
      }
    );
    document.querySelectorAll("[data-version]").forEach((node) => {
      node.addEventListener("click", async () => {
        const path = await safeCall("restore_version", {
          id: S.projectId,
          versionId: node.dataset.version,
        });
        closeModal();
        await openFile(path);
        toast("Restored " + shortPath(path), "ok");
      });
    });
    document.querySelectorAll("[data-backup]").forEach((node) => {
      node.addEventListener("click", async () => {
        if (!window.confirm("Restore this full backup? Current files are overwritten (snapshots are kept).")) return;
        const count = await safeCall("restore_backup", { id: S.projectId, backupId: node.dataset.backup });
        closeModal();
        await refreshTree();
        toast("Restored " + count + " files", "ok");
      });
    });
  }

  // ------------------------------------------------------------------- editor

  function setupEditor() {
    S.editor = Editor.create({
      textarea: $("code"),
      highlight: $("highlight"),
      highlightCode: $("highlightCode"),
      gutterInner: $("gutterInner"),
      tabSize: () => (S.settings ? S.settings.editor.tabSize : 4),
    });
    S.editor.onChange(() => {
      if (!S.activePath) return;
      if (!S.dirty) {
        S.dirty = true;
        setSaveState("dirty", "Unsaved");
        S.tabs.forEach((tab) => { if (tab.path === S.activePath) tab.dirty = true; });
        renderTabs();
      }
      clearTimeout(analyseTimer);
      analyseTimer = setTimeout(() => analyseActive(false), 900);
    });
    $("code").addEventListener("keydown", (event) => {
      if (event.key === "Tab") event.stopPropagation();
    });
  }

  function autosaveTick() {
    if (S.dirty && S.activePath) saveActiveFile(true);
  }

  function toggleBottomPanel() {
    const panel = $("bottomPanel");
    panel.classList.toggle("collapsed");
    const collapsed = panel.classList.contains("collapsed");
    const settings = { ...S.settings };
    settings.ui = { ...settings.ui, showBottomPanel: !collapsed };
    S.settings = settings;
    call("update_settings", { settings }).catch(() => {});
  }

  function setBottomTab(panel) {
    S.bottomPanel = panel;
    document.querySelectorAll(".btab").forEach((tab) => {
      tab.classList.toggle("is-active", tab.dataset.panel === panel);
    });
    renderBottom();
  }

  function renderBottom() {
    if (S.bottomPanel === "output") renderLogs();
    else if (S.bottomPanel === "diagnostics") renderDiagnosticsPanel();
    else if (S.bottomPanel === "activity") renderActivityPanel();
    else renderStudioPanel();
  }

  function renderLogs() {
    const body = $("bottomBody");
    if (S.bottomPanel !== "output") return;
    body.innerHTML = "";
    if (!S.logs.length) {
      body.appendChild(el("div", "log-line debug", '<span class="log-msg">No output yet.</span>'));
      return;
    }
    S.logs.slice(-400).forEach((entry) => body.appendChild(logNode(entry)));
    body.scrollTop = body.scrollHeight;
  }

  function logNode(entry) {
    const node = el("div", "log-line " + entry.level);
    node.innerHTML =
      '<span class="log-time">' + timeOf(entry.at) + "</span>" +
      '<span class="log-src">' + entry.source + "</span>" +
      '<span class="log-msg"></span>';
    node.querySelector(".log-msg").textContent = entry.message;
    if (entry.detail) {
      const detail = el("div", "log-detail");
      detail.textContent = entry.detail;
      node.appendChild(detail);
    }
    return node;
  }

  function appendLogLine(entry) {
    if (S.bottomPanel !== "output") return;
    const body = $("bottomBody");
    body.appendChild(logNode(entry));
    while (body.children.length > 400) body.removeChild(body.firstChild);
    body.scrollTop = body.scrollHeight;
  }

  function renderDiagnosticsPanel() {
    const body = $("bottomBody");
    body.innerHTML = "";
    const review = S.projectReview;
    if (review) {
      const head = el("div", "studio-tools-head");
      const title = el("span", "",
        "Project scan · " + review.errors + " error(s), " + review.warnings + " warning(s) in " + review.scanned + " file(s)");
      const rescan = el("button", "btn ghost sm", "Rescan");
      rescan.addEventListener("click", () => scanProject());
      head.appendChild(title);
      head.appendChild(rescan);
      body.appendChild(head);
      if (!review.files.length) {
        body.appendChild(el("div", "log-line debug", '<span class="log-msg">No static issues in this project.</span>'));
        return;
      }
      review.files.forEach((file) => {
        const group = el("div", "diag-file");
        const fileHead = el("div", "diag-file-head");
        fileHead.textContent = file.path + "  ·  " + file.diagnostics.length;
        group.appendChild(fileHead);
        file.diagnostics.forEach((diagnostic) => {
          const row = el("div", "log-line " + (diagnostic.severity === "error" ? "error" : diagnostic.severity === "warning" ? "warn" : "debug"));
          row.style.cursor = "pointer";
          row.innerHTML = '<span class="log-src">L' + diagnostic.line + "</span><span class='log-msg'></span>";
          row.querySelector(".log-msg").textContent = diagnostic.message + "  [" + diagnostic.code + "]";
          row.addEventListener("click", () => openFile(file.path, diagnostic.line));
          group.appendChild(row);
        });
        body.appendChild(group);
      });
      return;
    }
    if (!S.diagnostics.length) {
      body.appendChild(el("div", "log-line debug", '<span class="log-msg">No diagnostics for the active file. Use the shield button to scan the whole project.</span>'));
      return;
    }
    S.diagnostics.forEach((diagnostic) => {
      const row = el("div", "log-line " + (diagnostic.severity === "error" ? "error" : diagnostic.severity === "warning" ? "warn" : "debug"));
      row.style.cursor = "pointer";
      row.innerHTML = '<span class="log-src">L' + diagnostic.line + "</span><span class='log-msg'></span>";
      row.querySelector(".log-msg").textContent = diagnostic.message + "  [" + diagnostic.code + "]";
      row.addEventListener("click", () => openFile(S.activePath, diagnostic.line));
      body.appendChild(row);
    });
  }

  function renderActivityPanel() {
    const body = $("bottomBody");
    body.innerHTML = "";
    if (!S.activity.length) {
      body.appendChild(el("div", "log-line debug", '<span class="log-msg">No MCP tool activity yet.</span>'));
      return;
    }
    S.activity.slice(-200).forEach((event) => {
      const row = el("div", "activity-row");
      if (event.phase === "start") {
        row.innerHTML = '<span class="an"></span><span class="ad"></span>';
        row.querySelector(".an").textContent = "→ " + event.tool;
        row.querySelector(".ad").textContent = event.detail || "";
      } else {
        row.innerHTML = '<span class="' + (event.ok ? "ok" : "bad") + '"></span><span class="ad"></span>';
        row.querySelector("span").textContent = (event.ok ? "✓ " : "✗ ") + event.tool;
        row.querySelector(".ad").textContent =
          (event.durationMs ? event.durationMs + " ms" : "") + (event.detail ? " · " + event.detail : "");
      }
      body.appendChild(row);
    });
    body.scrollTop = body.scrollHeight;
  }

  function renderStudioPanel() {
    if (S.bottomPanel !== "studio") return;
    const body = $("bottomBody");
    const status = S.mcpStatus || { state: "disconnected" };
    const report = S.studioReport || {};
    const rows = [
      ["MCP state", statusLabel(status)],
      ["Server", status.server || "—"],
      ["Transport", status.transport || (report.launcher ? report.launcher.kind : "—")],
      ["Tools", status.toolCount !== undefined ? String(status.toolCount) : "0"],
      ["Studio id", status.studioId || "auto"],
      ["Studio installed", report.studioInstalled ? "yes" : "no"],
      ["Studio running", report.studioRunning ? "yes" : "no"],
      ["Studio version", report.studioVersion || "—"],
      ["Launcher", report.launcher ? report.launcher.kind : "not found"],
      ["Rojo", S.boot && S.boot.rojo && S.boot.rojo.installed ? (S.boot.rojo.version || "installed") : "not installed"],
    ];
    body.innerHTML = '<dl class="studio-grid">' +
      rows.map(([k, v]) => "<dt>" + MD.escape(k) + "</dt><dd>" + MD.escape(String(v)) + "</dd>").join("") +
      "</dl>";
    if (status.state === "error") {
      body.appendChild(el("div", "log-line error", '<span class="log-msg"></span>')).querySelector(".log-msg").textContent = status.message;
      if (status.hint) body.appendChild(el("div", "log-detail", MD.escape(status.hint)));
    }
    const actions = el("div", "");
    actions.style.marginTop = "10px";
    actions.style.display = "flex";
    actions.style.gap = "6px";
    const reconnect = el("button", "btn", '<svg class="ic"><use href="#i-plug"/></svg>Reconnect MCP');
    reconnect.addEventListener("click", reconnectMcp);
    const launch = el("button", "btn", '<svg class="ic"><use href="#i-play"/></svg>Launch Studio');
    launch.addEventListener("click", async () => {
      try {
        await call("studio_launch");
        toast("Launching Roblox Studio…", "ok");
      } catch (error) {
        toast(errorText(error), "error");
      }
    });
    const diag = el("button", "btn ghost", "Diagnostics");
    diag.addEventListener("click", mcpDiagnosticsModal);
    actions.appendChild(reconnect);
    actions.appendChild(launch);
    actions.appendChild(diag);
    body.appendChild(actions);
    body.appendChild(studioTestingBlock());
    refreshPlayState();
  }

  // -------------------------------------------------------- studio testing

  function studioTestingBlock() {
    const wrap = el("div", "studio-tools");
    wrap.innerHTML =
      '<div class="studio-tools-head"><span>Testing</span><span class="tag" id="playState">unknown</span></div>' +
      '<div class="studio-actions">' +
      '<button class="btn" data-play="start_play">Play</button>' +
      '<button class="btn" data-play="run_server">Run server</button>' +
      '<button class="btn ghost" data-play="stop">Stop</button>' +
      '<button class="btn ghost" id="studioStateBtn">State</button>' +
      '<button class="btn ghost" id="studioConsoleBtn">Console</button>' +
      '<button class="btn ghost" id="studioShotBtn">Screenshot</button>' +
      "</div>" +
      '<pre class="studio-out" id="studioOut" hidden></pre>' +
      '<div class="shot-strip" id="shotStrip"></div>';
    wrap.querySelectorAll("[data-play]").forEach((button) => {
      button.addEventListener("click", () => studioPlay(button.dataset.play));
    });
    wrap.querySelector("#studioStateBtn").addEventListener("click", () => studioState());
    wrap.querySelector("#studioConsoleBtn").addEventListener("click", () => studioConsole());
    wrap.querySelector("#studioShotBtn").addEventListener("click", () => studioScreenshot());
    renderShots(wrap.querySelector("#shotStrip"));
    return wrap;
  }

  function summarizeState(text) {
    const raw = String(text || "").trim();
    try {
      const parsed = JSON.parse(raw);
      const mode = parsed.mode || parsed.playState || parsed.play_state || parsed.state;
      if (typeof mode === "string") return mode;
      if (typeof parsed.isRunning === "boolean") return parsed.isRunning ? "running" : "edit";
    } catch (error) {
      /* plain text is fine */
    }
    return raw.replace(/\s+/g, " ").slice(0, 42) || "unknown";
  }

  async function refreshPlayState() {
    const targets = [$("playState"), $("testingStatus")].filter(Boolean);
    if (!targets.length) return;
    const status = S.mcpStatus || { state: "disconnected" };
    let label = status.state;
    try {
      label = summarizeState(await call("studio_state"));
    } catch (error) {
      label = status.state === "connected" ? "state unavailable" : status.state;
    }
    const clean = label ? label.charAt(0).toUpperCase() + label.slice(1) : label;
    targets.forEach((node) => {
      node.textContent = node.id === "testingStatus" && status.state === "connected"
        ? "Connected · " + clean
        : clean;
    });
  }

  async function studioPlay(mode) {
    const out = $("studioOut");
    if (out) {
      out.hidden = false;
      out.textContent = "Requesting " + mode + "…";
    }
    try {
      const text = await call("studio_play", { mode });
      if (out) out.textContent = text;
      toast(mode === "stop" ? "Playtest stopped" : "Studio: " + mode.replace("_", " "), "ok");
      refreshPlayState();
    } catch (error) {
      if (out) out.textContent = errorText(error);
      toast(errorText(error), "error");
    }
  }

  async function studioState() {
    const out = $("studioOut");
    if (out) {
      out.hidden = false;
      out.textContent = "Reading Studio state…";
    }
    try {
      const text = await call("studio_state");
      if (out) out.textContent = text;
      refreshPlayState();
    } catch (error) {
      if (out) out.textContent = errorText(error);
      toast(errorText(error), "error");
    }
  }

  async function studioConsole() {
    const out = $("studioOut");
    if (out) {
      out.hidden = false;
      out.textContent = "Reading the Studio console…";
    }
    try {
      const text = await call("studio_console");
      if (out) out.textContent = text;
    } catch (error) {
      if (out) out.textContent = errorText(error);
      toast(errorText(error), "error");
    }
  }

  async function studioScreenshot() {
    const btn = $("studioShotBtn");
    if (btn) btn.disabled = true;
    try {
      const shot = await call("studio_screenshot");
      shot.at = new Date().toISOString();
      S.screenshots.unshift(shot);
      S.screenshots = S.screenshots.slice(0, 8);
      renderShots();
      toast("Screenshot saved · " + Math.round(shot.bytes / 1024) + " KB", "ok");
    } catch (error) {
      toast(errorText(error), "error");
      const out = $("studioOut");
      if (out) {
        out.hidden = false;
        out.textContent = errorText(error);
      }
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  function renderShots(target) {
    const strips = target ? [target] : [$("shotStrip"), $("testShots")].filter(Boolean);
    strips.forEach((strip) => {
      strip.innerHTML = "";
      S.screenshots.forEach((shot) => {
        const button = el("button", "shot-thumb");
        button.innerHTML = "<img alt='Studio viewport'><span></span>";
        button.querySelector("img").src = shot.dataUrl;
        button.querySelector("span").textContent = timeOf(shot.at || new Date().toISOString());
        button.addEventListener("click", () => openShot(shot));
        strip.appendChild(button);
      });
    });
  }

  function openShot(shot) {
    openModal(
      "<h2>Studio screenshot</h2><p class='sub'>" + MD.escape(shot.path) + "</p>" +
        "<div class='shot-full'><img alt='Studio viewport' src='" + shot.dataUrl + "'></div>",
      {
        actions: [
          {
            label: "Copy path",
            kind: "ghost",
            handler: async () => {
              try {
                await navigator.clipboard.writeText(shot.path);
                toast("Path copied", "ok");
              } catch (error) {
                toast("Clipboard unavailable", "error");
              }
            },
          },
          { label: "Close", handler: closeModal },
        ],
      }
    );
  }

  async function refreshStudio() {
    try {
      S.studioReport = await call("studio_discover");
      if (S.boot) S.boot.studio = S.studioReport;
      if (S.bottomPanel === "studio") renderStudioPanel();
      renderWelcomeStatus();
    } catch (error) {
      /* Studio probing is best-effort. */
    }
  }

  function renderWelcomeStatus() {
    if (!$("welcome").hidden && S.studioReport) {
      $("welcomeStatus").innerHTML =
        "Studio: <strong>" + (S.studioReport.studioInstalled ? "installed" : "not found") + "</strong> · " +
        "MCP: <strong>" + statusLabel(S.mcpStatus) + "</strong>" +
        (S.studioReport.notes && S.studioReport.notes.length
          ? "<br/><span style='color:var(--faint)'>" + MD.escape(S.studioReport.notes[0]) + "</span>"
          : "");
    }
  }

  async function reconnectMcp() {
    $("studioLabel").textContent = "Studio: connecting…";
    setStatusDot("connecting");
    try {
      const status = await call("mcp_reconnect");
      S.mcpStatus = status;
      renderStatus();
      if (status.state === "connected") toast("Connected to " + status.server, "ok");
    } catch (error) {
      toast(errorText(error), "error");
      S.mcpStatus = { state: "error", message: errorText(error) };
      renderStatus();
    }
  }

  async function mcpDiagnosticsModal() {
    const diagnostics = await safeCall("mcp_diagnostics");
    const report = diagnostics.report || {};
    const logs = (diagnostics.recentLogs || []).slice(-40)
      .map((entry) => "<div class='log-line " + entry.level + "'><span class='log-time'>" + timeOf(entry.at) +
        "</span><span class='log-src'>" + entry.source + "</span><span class='log-msg'>" + MD.escape(entry.message) + "</span></div>")
      .join("");
    openModal(
      "<h2>MCP diagnostics</h2><p class='sub'>" + MD.escape(statusLabel(diagnostics.status)) + "</p>" +
      "<dl class='kv'>" +
      "<dt>Protocol</dt><dd>" + MD.escape(diagnostics.protocolVersion || "—") + "</dd>" +
      "<dt>Server</dt><dd>" + MD.escape(diagnostics.server ? diagnostics.server.name + " " + diagnostics.server.version : "—") + "</dd>" +
      "<dt>Tools</dt><dd>" + diagnostics.toolCount + "</dd>" +
      "<dt>Studio launcher</dt><dd>" + MD.escape(report.launcher ? report.launcher.kind : "not found") + "</dd>" +
      "<dt>Studio installed</dt><dd>" + (report.studioInstalled ? "yes" : "no") + "</dd>" +
      "<dt>Checked paths</dt><dd>" + MD.escape((report.checked || []).join(" · ") || "—") + "</dd>" +
      "</dl>" +
      "<div class='field' style='margin-top:16px'><label>Recent MCP log</label>" +
      "<div style='max-height:220px;overflow:auto;font-family:var(--mono);font-size:11px'>" + (logs || "<span class='sub'>empty</span>") + "</div></div>",
      { actions: [{ label: "Close", kind: "primary", handler: closeModal }] }
    );
  }

  // ------------------------------------------------------------------- status

  function statusLabel(status) {
    if (!status) return "Disconnected";
    switch (status.state) {
      case "connected": return "Connected · " + (status.toolCount || 0) + " tools";
      case "connecting": return "Connecting…";
      case "reconnecting": return "Reconnecting (attempt " + status.attempt + ")";
      case "error": return "Error: " + status.message;
      default: return "Disconnected";
    }
  }

  function setStatusDot(kind) {
    const dot = $("studioDot");
    dot.className = "status-dot " + (kind || "");
  }

  function renderStatus() {
    const status = S.mcpStatus || { state: "disconnected" };
    setStatusDot(status.state);
    $("studioLabel").textContent = "Studio: " + (status.state === "connected" ? "connected" : status.state);
    $("studioPill").title = statusLabel(status) + (status.transport ? "\n" + status.transport : "");
    const chip = $("ctxStudio");
    chip.classList.toggle("live", status.state === "connected");
    chip.innerHTML = '<svg class="ic"><use href="#i-plug"/></svg>' +
      (status.state === "connected" ? "Studio connected" : "Studio " + status.state);
    renderTools();
    renderWelcomeStatus();
    updateUsage();
  }

  function shortModel(id) {
    if (!id) return "no model";
    const parts = id.split("/");
    return parts.length > 1 ? parts[parts.length - 1] : id;
  }

  function renderModelLabel() {
    const model = S.settings ? S.settings.ai.model : "";
    $("modelLabel").textContent = model ? shortModel(model) : "Choose a model";
    $("modelDot").classList.toggle("on", !!model);
    $("assistantModel").textContent = model ? shortModel(model) : "no model";
  }

  function closeModelMenu() {
    const node = $("modelPopover");
    if (node) node.remove();
  }

  function toggleModelMenu(anchor) {
    const open = $("modelPopover");
    if (open && open.dataset.anchor === anchor.id) {
      closeModelMenu();
      return;
    }
    openModelMenu(anchor);
  }

  // One selector, anchored to whichever control asked for it.
  function openModelMenu(anchor) {
    closeModelMenu();
    const rect = anchor.getBoundingClientRect();
    const popover = el("div", "popover model-popover");
    popover.id = "modelPopover";
    popover.dataset.anchor = anchor.id;
    popover.innerHTML =
      '<div class="popover-head"><span class="popover-brand"><img src="assets/terrouter.svg" alt="">' +
      "<span>Ter Router models</span></span>" +
      '<button class="icon-btn sm" id="refreshModels" title="Refresh model list">' +
      '<svg class="ic"><use href="#i-refresh"/></svg></button></div>' +
      '<div class="input-row"><svg class="ic"><use href="#i-search"/></svg>' +
      '<input id="modelFilter" placeholder="Filter models…" autocomplete="off"></div>' +
      '<div class="model-list" id="modelList"></div>';
    document.body.appendChild(popover);

    const width = popover.offsetWidth || 400;
    const left = Math.min(Math.max(8, rect.left + rect.width / 2 - width / 2), window.innerWidth - width - 8);
    const top = Math.max(8, Math.min(rect.bottom + 8, window.innerHeight - popover.offsetHeight - 8));
    popover.style.left = left + "px";
    popover.style.top = top + "px";

    $("modelFilter").value = S.modelFilter || "";
    $("modelFilter").addEventListener("input", (event) => {
      S.modelFilter = event.target.value.toLowerCase();
      renderModelList();
    });
    $("refreshModels").addEventListener("click", (event) => {
      event.stopPropagation();
      refreshModels();
    });
    renderModelList();
    $("modelFilter").focus();
    if (!S.models.length) refreshModels();
  }

  function renderModelList() {
    const list = $("modelList");
    if (!list) return;
    list.innerHTML = "";
    const models = S.models.filter((m) => !S.modelFilter || m.id.toLowerCase().includes(S.modelFilter));
    if (!models.length) {
      list.appendChild(el("div", "side-empty", S.models.length ? "No models match that filter." : "No models loaded. Add your API key, then refresh."));
      return;
    }
    models.forEach((model) => {
      const item = el("button", "model-item" + (S.settings.ai.model === model.id ? " is-current" : ""));
      item.innerHTML = '<span class="mname"></span><span class="mmeta"></span>';
      item.querySelector(".mname").textContent = model.id;
      item.querySelector(".mmeta").textContent = model.context_length ? Math.round(model.context_length / 1000) + "k" : "";
      item.addEventListener("click", () => selectModel(model.id));
      list.appendChild(item);
    });
  }

  async function selectModel(id) {
    const settings = { ...S.settings };
    settings.ai = { ...settings.ai, model: id };
    S.settings = settings;
    renderModelLabel();
    closeModelMenu();
    await safeCall("update_settings", { settings });
    toast("Model set to " + shortModel(id), "ok");
  }

  async function refreshModels() {
    try {
      const models = await safeCall("list_models", { refresh: true });
      S.models = models;
      renderModelList();
      toast(models.length + " models available", "ok");
      if (!S.settings.ai.model && models.length) {
        const preferred = models.find((m) => /claude|gpt|gemini/i.test(m.id)) || models[0];
        await selectModel(preferred.id);
      }
    } catch (error) {
      /* surfaced by safeCall */
    }
  }

  function updateUsage() {
    const usage = S.usage || {};
    $("usageLabel").textContent = (usage.total_tokens || 0).toLocaleString() + " tokens";
  }

  // --------------------------------------------------------------- self-test

  function applyEditorPrefs() {
    if (!S.settings) return;
    const editor = S.settings.editor || {};
    const size = (editor.fontSize || 13) + "px";
    ["code", "highlight", "gutterInner"].forEach((id) => {
      const node = $(id);
      if (node) node.style.fontSize = size;
    });
    const wrap = !!editor.wordWrap;
    $("workspace").classList.toggle("wrap", wrap);
    $("code").wrap = wrap ? "soft" : "off";
    if (S.editor) S.editor.refresh();
  }

  function updateAutoApproveChip() {
    const on = !!(S.settings && S.settings.mcp && S.settings.mcp.autoApprove);
    $("autoApproveChip").classList.toggle("on", on);
  }

  function verdictLabel(verdict) {
    if (verdict === "pass") return "PASS";
    if (verdict === "notes") return "PASS WITH NOTES";
    if (verdict === "fail") return "FAIL";
    return verdict ? "NO VERDICT" : "idle";
  }

  function checkIcon(status) {
    if (status === "pass") return "i-check";
    if (status === "fail") return "i-x";
    if (status === "warn") return "i-alert";
    return "i-chevron";
  }

  function renderTestChecks() {
    const container = $("testChecks");
    if (!container) return;
    container.innerHTML = "";
    const checks = S.selfTest.checks || [];
    if (!checks.length) {
      const empty = el("div", "test-check skip");
      empty.innerHTML =
        '<span class="tc-icon"><svg class="ic"><use href="#i-shield"/></svg></span>' +
        '<div class="tc-body"><div class="tc-label">No checks yet</div>' +
        '<div class="tc-detail">Scan the project or run a self-test to collect real results. Nothing here is estimated.</div></div>';
      container.appendChild(empty);
      return;
    }
    checks.forEach((check) => {
      const status = check.status || "skip";
      const card = el("div", "test-check " + status);
      card.innerHTML =
        '<span class="tc-icon"><svg class="ic"><use href="#' + checkIcon(status) + '"/></svg></span>' +
        '<div class="tc-body"><div class="tc-label"><span></span></div><div class="tc-detail"></div></div>';
      card.querySelector(".tc-label span").textContent = check.label || check.id || "Check";
      const tag = el("span", "tag " + (status === "pass" ? "ok" : status === "fail" ? "bad" : status === "warn" ? "warn" : ""), status);
      card.querySelector(".tc-label").appendChild(tag);
      card.querySelector(".tc-detail").textContent = check.detail || "";
      container.appendChild(card);
    });
  }

  function renderSelfTest() {
    renderTestChecks();
    const state = $("testRunState");
    if (!state) return;
    const running = S.selfTest.running;
    const verdict = S.selfTest.verdict;
    state.textContent = running
      ? "running"
      : verdict
        ? verdictLabel(verdict)
        : S.selfTest.checks.length
          ? "ready"
          : "idle";
    state.className =
      "tag " +
      (running ? "warn" : verdict === "pass" ? "ok" : verdict === "fail" ? "bad" : verdict ? "warn" : "");
    $("runSelfTest").disabled = running;
    $("testScan").disabled = running;
    $("runTestBtn").disabled = running;
    if (!S.selfTest.summary) $("testVerdict").hidden = true;
  }

  function clearTestLog() {
    const log = $("testLog");
    if (!log) return;
    log.innerHTML = "";
    S.selfTest.streamEl = null;
    S.selfTest.streamText = "";
    S.selfTest.streamQueue = "";
  }

  function appendTestPhase(label) {
    const log = $("testLog");
    if (!log) return;
    const empty = log.querySelector(".empty");
    if (empty) empty.remove();
    const node = el("div", "tl-phase");
    node.textContent = label;
    log.appendChild(node);
    log.scrollTop = log.scrollHeight;
  }

  function appendTestText(text) {
    const log = $("testLog");
    if (!log) return;
    const empty = log.querySelector(".empty");
    if (empty) empty.remove();
    const node = el("div", "tl-text");
    node.textContent = text;
    log.appendChild(node);
    log.scrollTop = log.scrollHeight;
  }

  let testFrame = null;
  function flushTestStream() {
    if (!S.selfTest.streamEl && !S.selfTest.streamQueue) return;
    const log = $("testLog");
    if (!S.selfTest.streamEl) {
      const empty = log.querySelector(".empty");
      if (empty) empty.remove();
      S.selfTest.streamEl = el("div", "tl-text");
      log.appendChild(S.selfTest.streamEl);
    }
    S.selfTest.streamText += S.selfTest.streamQueue;
    S.selfTest.streamQueue = "";
    S.selfTest.streamEl.innerHTML = MD.render(S.selfTest.streamText);
    log.scrollTop = log.scrollHeight;
  }

  function appendTestTool(id, name, args) {
    const log = $("testLog");
    const chip = el("div", "tool-chip running");
    chip.dataset.id = id || "";
    chip.dataset.tool = name;
    chip.innerHTML =
      '<span class="spinner"></span><span class="tname"></span><span class="tmeta"></span>';
    chip.querySelector(".tname").textContent = name;
    const compact = JSON.stringify(args || {});
    chip.querySelector(".tmeta").textContent = compact.length > 46 ? compact.slice(0, 46) + "…" : compact;
    log.appendChild(chip);
    log.scrollTop = log.scrollHeight;
  }

  function finishTestTool(event) {
    const chips = Array.from($("testLog").querySelectorAll(".tool-chip.running"));
    const chip = event.id ? chips.find((node) => node.dataset.id === event.id) : chips[chips.length - 1];
    if (!chip) return;
    chip.classList.remove("running");
    chip.classList.add(event.ok ? "ok" : "failed");
    const spinner = chip.querySelector(".spinner");
    if (spinner) {
      spinner.outerHTML =
        '<svg class="ic" style="width:13px;height:13px"><use href="#' +
        (event.ok ? "i-check" : "i-x") +
        '"/></svg>';
    }
    chip.querySelector(".tmeta").textContent = (event.durationMs || 0) + " ms";
    chip.title = event.summary || "";
  }

  function finishSelfTest(event) {
    S.selfTest.running = false;
    $("sendPrompt").disabled = false;
    S.selfTest.verdict = event.verdict || "unknown";
    S.selfTest.summary = event.summary || "Self-test finished.";
    flushTestStream();
    appendTestPhase("Result");
    appendTestText(S.selfTest.summary);
    const verdict = $("testVerdict");
    verdict.hidden = false;
    verdict.className =
      "test-verdict " + (event.verdict === "pass" ? "pass" : event.verdict === "fail" ? "fail" : "warn");
    verdict.innerHTML =
      '<svg class="ic"><use href="#' + (event.verdict === "fail" ? "i-x" : "i-check") + '"/></svg><span></span>';
    const parts = [verdictLabel(event.verdict)];
    if (event.errors !== undefined) parts.push(event.errors + " error(s)");
    if (event.toolCalls !== undefined) parts.push(event.toolCalls + " tool call(s)");
    verdict.querySelector("span").textContent = parts.join(" · ");
    renderSelfTest();
    toast("Self-test: " + verdictLabel(event.verdict), event.verdict === "fail" ? "error" : "ok");
  }

  function handleSelfTestEvent(event) {
    if (!event || !event.type) return;
    switch (event.type) {
      case "started": {
        S.selfTest.running = true;
        S.selfTest.verdict = null;
        S.selfTest.summary = "";
        S.selfTest.checks = event.checks || [];
        clearTestLog();
        if (event.review) {
          S.projectReview = event.review;
          renderDiagnostics();
          if (S.bottomPanel === "diagnostics") renderDiagnosticsPanel();
        }
        appendTestPhase("Static analysis");
        const review = event.review || { scanned: 0, errors: 0, warnings: 0 };
        appendTestText(
          "Analysed " + review.scanned + " Luau file(s): " + review.errors + " error(s), " + review.warnings + " warning(s)."
        );
        renderSelfTest();
        break;
      }
      case "phase":
        S.selfTest.running = true;
        appendTestPhase(event.label || "Working");
        renderSelfTest();
        break;
      case "delta":
        S.selfTest.streamQueue += event.text || "";
        if (!testFrame) {
          testFrame = requestAnimationFrame(() => {
            testFrame = null;
            flushTestStream();
          });
        }
        break;
      case "round":
        $("testRunState").textContent = "step " + event.round;
        break;
      case "toolStart":
        flushTestStream();
        S.selfTest.streamEl = null;
        appendTestTool(event.id, event.name, event.arguments);
        break;
      case "toolEnd":
        finishTestTool(event);
        break;
      case "usage":
        S.usage = { total_tokens: event.total };
        updateUsage();
        break;
      case "done":
        flushTestStream();
        break;
      case "failed":
        S.selfTest.running = false;
        appendTestPhase("Stopped");
        appendTestText(event.message || "The run stopped.");
        renderSelfTest();
        break;
      case "confirmation":
        // A self-test never waits for a human; say so instead of drawing a
        // card that nobody can resolve.
        appendTestPhase("Auto-approved");
        break;
      case "finished":
        finishSelfTest(event);
        break;
      case "error":
        S.selfTest.running = false;
        $("sendPrompt").disabled = false;
        flushTestStream();
        appendTestPhase("Error");
        appendTestText(event.message || "The self-test failed.");
        renderSelfTest();
        toast(event.message || "Self-test failed", "error");
        break;
      default:
        break;
    }
  }

  async function scanProject(options) {
    if (!S.projectId) {
      toast("Open a project first", "error");
      return null;
    }
    try {
      const review = await call("analyze_project", { id: S.projectId });
      S.projectReview = review;
      renderDiagnostics();
      if (options && options.openPanel) {
        $("bottomPanel").classList.remove("collapsed");
        setBottomTab("diagnostics");
      } else if (S.bottomPanel === "diagnostics") {
        renderDiagnosticsPanel();
      }
      if (!options || !options.silent) {
        toast(
          review.errors + " error(s), " + review.warnings + " warning(s) in " + review.scanned + " file(s)",
          review.errors ? "error" : "ok"
        );
      }
      return review;
    } catch (error) {
      toast(errorText(error), "error");
      return null;
    }
  }

  async function scanSelfTestChecks() {
    if (!S.projectId) {
      toast("Open a project first", "error");
      return;
    }
    if (S.selfTest.running) return;
    const review = await scanProject({ silent: true });
    if (!review) return;
    const status = S.mcpStatus || { state: "disconnected" };
    const project = S.projects.find((p) => p.id === S.projectId);
    S.selfTest.checks = [
      {
        id: "project",
        label: "Project",
        status: "pass",
        detail: (project ? project.name : "Project") + " · " + review.scanned + " Luau file(s) analysed",
      },
      {
        id: "luau",
        label: "Luau static analysis",
        status: review.errors ? "fail" : "pass",
        detail: review.errors + " error(s), " + review.warnings + " warning(s) in " + review.files.length + " file(s)",
      },
      {
        id: "provider",
        label: "AI provider",
        status: S.settings.ai.model ? "pass" : "fail",
        detail: (S.settings.ai.baseUrl || "") + (S.settings.ai.model ? " · " + S.settings.ai.model : " · no model selected"),
      },
      {
        id: "studio",
        label: "Roblox Studio MCP",
        status: status.state === "connected" ? "pass" : "warn",
        detail:
          status.state === "connected"
            ? "Connected · " + (S.tools.length || 0) + " tool(s) available"
            : statusLabel(status) + " — live verification is skipped",
      },
    ];
    appendTestPhase("Scan");
    appendTestText(
      "Static scan complete: " + review.scanned + " file(s), " + review.errors + " error(s), " + review.warnings + " warning(s)."
    );
    renderSelfTest();
  }

  async function runSelfTest() {
    if (S.selfTest.running) {
      toast("A self-test is already running", "error");
      return;
    }
    if (S.agentBusy) {
      toast("Wait for the current assistant request to finish", "error");
      return;
    }
    if (!S.projectId) {
      toast("Open a project before running a self-test", "error");
      return;
    }
    if (!S.settings.ai.model) {
      toast("Choose a model first", "error");
      openSettings("ai");
      return;
    }
    clearTestLog();
    S.selfTest.checks = [];
    S.selfTest.running = true;
    S.selfTest.verdict = null;
    S.selfTest.summary = "";
    $("testVerdict").hidden = true;
    appendTestPhase("Starting");
    appendTestText(
      "Running the static analysis, then handing the project to " +
        shortModel(S.settings.ai.model) +
        " with Studio tool calls auto-approved."
    );
    renderSelfTest();
    $("sendPrompt").disabled = true;
    try {
      await call("run_self_test", { projectId: S.projectId });
    } catch (error) {
      S.selfTest.running = false;
      $("sendPrompt").disabled = false;
      appendTestPhase("Error");
      appendTestText(errorText(error));
      renderSelfTest();
      toast(errorText(error), "error");
    }
  }

  // ---------------------------------------------------------- command palette

  function paletteEntries() {
    const entries = [
      { label: "Run AI self-test", hint: "Verify the project in Studio", run: () => { closeModal(); openSelfTest(); runSelfTest(); } },
      { label: "Scan project", hint: "Static-check every Luau file", run: () => { closeModal(); scanProject({ openPanel: true }); } },
      { label: "New project", hint: "Create one from a template", run: () => { closeModal(); newProjectModal(); } },
      { label: "New file", hint: "Add a script", run: () => { closeModal(); newFileModal(); } },
      { label: "Save file", hint: "Ctrl+S", run: () => { closeModal(); saveActiveFile(); } },
      { label: "Backup project", hint: "Snapshot every file", run: () => { closeModal(); backupProject(); } },
      { label: "Version history", hint: "Snapshots and backups", run: () => { closeModal(); versionsModal(); } },
      { label: "Export .rbxmx", hint: "Package the scripts", run: () => { closeModal(); exportModel(); } },
      { label: "Rojo build", hint: "Build a place file", run: () => { closeModal(); rojoBuild(); } },
      { label: "Reconnect Studio MCP", hint: "Ctrl+Shift+R", run: () => { closeModal(); reconnectMcp(); } },
      { label: "Studio diagnostics", hint: "MCP handshake and launcher", run: () => { closeModal(); mcpDiagnosticsModal(); } },
      { label: "Prompt history", hint: "Reuse an earlier request", run: () => { closeModal(); promptHistoryModal(); } },
      { label: "Settings: AI provider", hint: "Ter Router key and model", run: () => { closeModal(); openSettings("ai"); } },
      { label: "Settings: Roblox MCP", hint: "Approvals and connection", run: () => { closeModal(); openSettings("mcp"); } },
      { label: "Settings: Verification", hint: "Playtest and capture", run: () => { closeModal(); openSettings("testing"); } },
      { label: "Settings: About", hint: "Version and updates", run: () => { closeModal(); openSettings("about"); } },
      { label: "Toggle bottom panel", hint: "Ctrl+`", run: () => { closeModal(); toggleBottomPanel(); } },
    ];
    S.files.slice(0, 80).forEach((file) => {
      entries.push({ label: file, hint: "Open file", run: () => { closeModal(); openFile(file); } });
    });
    return entries;
  }

  function openPalette() {
    openModal(
      "<h2>Command palette</h2><p class='sub'>Run a command or open a file. ↑ ↓ to move, Enter to run.</p>" +
        "<div class='input-row'><svg class='ic'><use href='#i-search'/></svg>" +
        "<input id='paletteInput' placeholder='Type a command or file…' autocomplete='off'></div>" +
        "<div id='paletteList' style='max-height:340px;overflow:auto;margin-top:8px'></div>",
      { actions: [{ label: "Close", kind: "ghost", handler: closeModal }] }
    );
    const entries = paletteEntries();
    let matches = entries;
    let active = 0;
    const list = () => $("paletteList");

    const runActive = () => {
      const entry = matches[active];
      if (entry) entry.run();
    };

    const paint = (query) => {
      const needle = (query || "").toLowerCase();
      matches = entries.filter((entry) => !needle || entry.label.toLowerCase().includes(needle));
      if (active >= matches.length) active = 0;
      const listNode = list();
      listNode.innerHTML = "";
      if (!matches.length) {
        listNode.appendChild(el("p", "sub", "No command matches that."));
        return;
      }
      matches.forEach((entry, index) => {
        const row = el("button", "prompt-history-item" + (index === active ? " is-active" : ""));
        row.innerHTML = "<span class='pal-label'></span><span class='when'></span>";
        row.querySelector(".pal-label").textContent = entry.label;
        row.querySelector(".when").textContent = entry.hint || "";
        row.addEventListener("mouseenter", () => { active = index; });
        row.addEventListener("click", () => { active = index; runActive(); });
        listNode.appendChild(row);
      });
    };

    const input = $("paletteInput");
    input.addEventListener("input", () => { active = 0; paint(input.value); });
    input.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        active = Math.min(active + 1, matches.length - 1);
        paint(input.value);
        const current = list().querySelectorAll(".prompt-history-item")[active];
        if (current) current.scrollIntoView({ block: "nearest" });
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        active = Math.max(active - 1, 0);
        paint(input.value);
        const current = list().querySelectorAll(".prompt-history-item")[active];
        if (current) current.scrollIntoView({ block: "nearest" });
      } else if (event.key === "Enter") {
        event.preventDefault();
        runActive();
      }
    });
    paint("");
    input.focus();
  }

  async function backupProject() {
    if (!S.projectId) { toast("Open a project first", "error"); return; }
    try {
      const info = await call("backup_project", { id: S.projectId, label: "manual" });
      toast("Backup created (" + Math.round(info.bytes / 1024) + " KB)", "ok");
    } catch (error) {
      toast(errorText(error), "error");
    }
  }

  async function exportModel() {
    if (!S.projectId) { toast("Open a project first", "error"); return; }
    try {
      const path = await call("export_model", { id: S.projectId });
      toast("Exported " + shortPath(path), "ok");
    } catch (error) {
      toast(errorText(error), "error");
    }
  }

  async function rojoBuild() {
    if (!S.projectId) { toast("Open a project first", "error"); return; }
    try {
      const path = await call("rojo_build", { id: S.projectId });
      toast("Built " + shortPath(path), "ok");
    } catch (error) {
      toast(errorText(error), "error");
    }
  }

  // ----------------------------------------------------------------- assistant

  function bindMessages() {
    $("sendPrompt").addEventListener("click", sendPrompt);
    $("promptInput").addEventListener("keydown", (event) => {
      if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        sendPrompt();
      }
    });
    $("promptInput").addEventListener("input", () => {
      const input = $("promptInput");
      input.style.height = "auto";
      input.style.height = Math.min(input.scrollHeight, 140) + "px";
    });
    $("clearChat").addEventListener("click", async () => {
      await call("clear_conversation").catch(() => {});
      S.messages = [];
      S.usage = { total_tokens: 0 };
      $("messages").innerHTML =
        '<div class="msg-hint"><strong>New conversation</strong><span>Ask for a system, a fix or an inspection.</span></div>';
      updateUsage();
    });
    $("promptHistoryBtn").addEventListener("click", promptHistoryModal);
    $("attachContext").classList.add("is-active");
    $("attachContext").addEventListener("click", () => {
      S.attachActive = !S.attachActive;
      $("attachContext").classList.toggle("is-active", S.attachActive);
      $("attachContext").title = S.attachActive
        ? "Active file is included as context"
        : "Active file is not included";
      toast(S.attachActive ? "Active file included in prompts" : "Active file excluded", "ok");
    });
    document.addEventListener("click", (event) => {
      const chip = event.target.closest("[data-prompt]");
      if (chip) {
        $("promptInput").value = chip.dataset.prompt;
        $("promptInput").focus();
      }
      const external = event.target.closest("[data-external]");
      if (external) {
        event.preventDefault();
        call("open_external", { url: external.getAttribute("href") }).catch(() => {});
      }
      const codeAction = event.target.closest("[data-action]");
      if (codeAction) handleCodeAction(codeAction);
    });
  }

  async function handleCodeAction(button) {
    const block = button.closest(".code-block");
    if (!block) return;
    const code = block.querySelector("code").textContent;
    if (button.dataset.action === "copy-code") {
      try {
        await navigator.clipboard.writeText(code);
        toast("Copied to clipboard", "ok");
      } catch (error) {
        toast("Clipboard unavailable", "error");
      }
      return;
    }
    if (button.dataset.action === "insert-code") {
      if (!S.projectId) { toast("Open a project first", "error"); return; }
      openModal(
        "<h2>Insert code into project</h2><p class='sub'>Write the snippet to a file you choose.</p>" +
        "<div class='field'><label>Path</label><input type='text' id='icPath' placeholder='src/server/Generated.server.luau'></div>",
        {
          actions: [
            { label: "Cancel", kind: "ghost", handler: closeModal },
            {
              label: "Write file",
              kind: "primary",
              handler: async () => {
                const path = $("icPath").value.trim();
                if (!path) return;
                await safeCall("create_file", { id: S.projectId, path, contents: code });
                closeModal();
                await refreshTree();
                await openFile(path);
              },
            },
          ],
        }
      );
    }
  }

  function renderMessagesFromHistory() {
    const container = $("messages");
    if (!S.boot || !S.boot.history || !S.boot.history.length) return;
    container.innerHTML = "";
    S.boot.history.forEach((message) => {
      if (message.role === "user" && message.content) appendMessage("user", message.content, true);
      else if (message.role === "assistant" && message.content) appendMessage("assistant", message.content, true);
    });
  }

  function appendMessage(role, content, quiet) {
    const container = $("messages");
    const hint = container.querySelector(".msg-hint");
    if (hint) hint.remove();
    const wrapper = el("div", "msg " + role);
    wrapper.innerHTML =
      '<div class="msg-role">' + (role === "user" ? "You" : "TerCode") + "</div>" +
      '<div class="bubble"></div>';
    const bubble = wrapper.querySelector(".bubble");
    bubble.innerHTML = role === "assistant" ? MD.render(content) : MD.escape(content).replace(/\n/g, "<br/>");
    container.appendChild(wrapper);
    container.scrollTop = container.scrollHeight;
    if (!quiet && role === "user") {
      const chip = el("span", "t", timeOf(new Date().toISOString()));
      wrapper.querySelector(".msg-role").appendChild(chip);
    }
    return bubble;
  }

  function appendToolChip(tool, args) {
    const container = $("messages");
    const chip = el("div", "tool-chip running");
    chip.dataset.tool = tool;
    chip.innerHTML = '<span class="spinner"></span><span class="tname"></span><span class="tmeta"></span>';
    chip.querySelector(".tname").textContent = tool;
    const compact = JSON.stringify(args || {});
    chip.querySelector(".tmeta").textContent = compact.length > 42 ? compact.slice(0, 42) + "…" : compact;
    container.appendChild(chip);
    container.scrollTop = container.scrollHeight;
    return chip;
  }

  function renderConfirmation(request) {
    S.pendingConfirm = request;
    const container = $("messages");
    const card = el("div", "confirm-card");
    card.innerHTML =
      "<h4><svg class='ic'><use href='#i-alert'/></svg>Approve “" + MD.escape(request.tool) + "”?</h4>" +
      "<p>" + MD.escape(request.reason) + "</p>" +
      "<pre></pre>" +
      '<div class="confirm-actions">' +
      '<button class="btn primary" data-approve="1">Run it</button>' +
      '<button class="btn ghost" data-decline="1">Decline</button></div>';
    card.querySelector("pre").textContent = JSON.stringify(request.arguments, null, 2);
    card.querySelector("[data-approve]").addEventListener("click", () => resolveConfirm(request.id, true, card));
    card.querySelector("[data-decline]").addEventListener("click", () => resolveConfirm(request.id, false, card));
    container.appendChild(card);
    container.scrollTop = container.scrollHeight;
  }

  async function resolveConfirm(id, approved, card) {
    S.pendingConfirm = null;
    card.querySelector(".confirm-actions").innerHTML =
      '<span class="tag">' + (approved ? "approved" : "declined") + "</span>";
    await call("confirm_tool", { id, approved }).catch(() => {});
  }

  async function sendPrompt() {
    const input = $("promptInput");
    const text = input.value.trim();
    if (!text) return;
    if (S.selfTest.running) { toast("Wait for the self-test to finish", "error"); return; }
    if (S.agentBusy) { toast("Wait for the current request to finish", "error"); return; }
    if (!S.settings.ai.model) { toast("Choose a model first", "error"); openSettings("ai"); return; }
    input.value = "";
    input.style.height = "auto";
    appendMessage("user", text);
    S.streamEl = null;
    S.streamText = "";
    S.agentBusy = true;
    $("agentState").textContent = "Working…";
    $("agentState").className = "busy";
    $("sendPrompt").disabled = true;
    try {
      await call("send_message", {
        text,
        projectId: S.projectId,
        activePath: S.attachActive ? S.activePath : null,
      });
    } catch (error) {
      finishAgent();
      appendMessage("assistant", "**Could not start the request.** " + errorText(error));
      toast(errorText(error), "error");
    }
  }

  let streamFrame = null;
  function handleAgentEvent(event) {
    if (!event) return;
    switch (event.type) {
      case "round":
        $("agentState").textContent = "Working… (step " + event.round + ")";
        break;
      case "delta": {
        S.streamQueue += event.text;
        if (!S.streamEl) S.streamEl = { text: "" };
        if (!streamFrame) {
          streamFrame = requestAnimationFrame(() => {
            streamFrame = null;
            if (!S.streamEl) return;
            S.streamEl.text += S.streamQueue;
            S.streamQueue = "";
            paintStream();
          });
        }
        break;
      }
      case "toolStart":
        flushStream();
        // Start a fresh bubble after the tool chip so text and chips interleave
        // in the order they happened.
        S.streamEl = null;
        appendToolChip(event.name, event.arguments);
        break;
      case "toolEnd": {
        const chips = document.querySelectorAll(".tool-chip.running");
        const chip = chips[chips.length - 1];
        if (chip) {
          chip.classList.remove("running");
          chip.classList.add(event.ok ? "ok" : "failed");
          const spinner = chip.querySelector(".spinner");
          if (spinner) spinner.outerHTML = '<svg class="ic" style="width:13px;height:13px"><use href="#' +
            (event.ok ? "i-check" : "i-x") + '"/></svg>';
          chip.querySelector(".tmeta").textContent = event.durationMs + " ms";
          chip.title = event.summary || "";
        }
        break;
      }
      case "usage":
        S.usage = { total_tokens: event.total };
        updateUsage();
        break;
      case "failed":
        finishAgent();
        appendMessage("assistant", "**Stopped.** " + event.message);
        break;
      case "confirmation":
        flushStream();
        renderConfirmation(event.request);
        $("agentState").textContent = "Waiting for approval";
        break;
      case "done":
        flushStream();
        break;
      default:
        break;
    }
  }

  function flushStream() {
    if (S.streamEl && S.streamQueue) {
      S.streamEl.text += S.streamQueue;
      S.streamQueue = "";
      paintStream();
    }
  }

  function paintStream() {
    if (!S.streamEl) return;
    if (!S.streamEl.node) {
      S.streamEl.node = appendMessage("assistant", "", true);
    }
    S.streamEl.node.innerHTML = MD.render(S.streamEl.text);
    const container = $("messages");
    container.scrollTop = container.scrollHeight;
  }

  function finishAgent() {
    flushStream();
    S.streamEl = null;
    S.streamText = "";
    S.streamQueue = "";
    S.agentBusy = false;
    $("agentState").textContent = "";
    $("agentState").className = "";
    $("sendPrompt").disabled = false;
  }

  async function promptHistoryModal() {
    const history = await safeCall("prompt_history").catch(() => S.promptHistory);
    const rows = (history && history.length)
      ? history.map((entry) =>
          "<div class='prompt-history-item' data-prompt-text='" + MD.escape(entry.text).replace(/'/g, "&#39;") + "'>" +
          MD.escape(entry.text.slice(0, 220)) + "<span class='when'>" + relTime(entry.at) + "</span></div>").join("")
      : "<p class='sub'>No prompts yet.</p>";
    openModal(
      "<h2>Prompt history</h2><p class='sub'>Reuse a previous request.</p>" + rows,
      { actions: [{ label: "Close", kind: "primary", handler: closeModal }] }
    );
    document.querySelectorAll("[data-prompt-text]").forEach((node) => {
      node.addEventListener("click", () => {
        $("promptInput").value = node.dataset.promptText;
        closeModal();
        $("promptInput").focus();
      });
    });
  }

  function renderTemplatesBar() {
    const bar = $("promptTemplates");
    let saved = [];
    try {
      saved = JSON.parse(localStorage.getItem("tercode.promptTemplates") || "[]");
    } catch (error) {
      saved = [];
    }
    const defaults = [
      { label: "Inspect project", text: "Inspect the open place: list the services and key instances, then tell me what is missing for a playable game." },
      { label: "Add leaderboard", text: "Add a global leaderboard with an OrderedDataStore, a clean UI panel and weekly reset." },
      { label: "Optimise", text: "Find performance problems in my scripts (loops, RunService, remotes) and fix the worst three." },
    ];
    bar.innerHTML = "";
    const all = defaults.concat(saved);
    all.slice(0, 6).forEach((item) => {
      const chip = el("button", "chip", "");
      chip.textContent = item.label || item.text.slice(0, 24);
      chip.addEventListener("click", () => {
        $("promptInput").value = item.text;
        $("promptInput").focus();
      });
      bar.appendChild(chip);
    });
    const save = el("button", "chip", "+");
    save.title = "Save current prompt as a template";
    save.addEventListener("click", () => {
      const text = $("promptInput").value.trim();
      if (!text) { toast("Type a prompt first", "error"); return; }
      saved.push({ label: text.slice(0, 22), text });
      localStorage.setItem("tercode.promptTemplates", JSON.stringify(saved.slice(-6)));
      renderTemplatesBar();
      toast("Prompt template saved", "ok");
    });
    bar.appendChild(save);
  }

  // ------------------------------------------------------------------- tools

  function toolModal(tool) {
    openModal(
      "<h2>" + MD.escape(tool.name) + "</h2>" +
      "<p class='sub'>" + MD.escape(tool.description || tool.title || "") + "</p>" +
      "<div class='field'><label>Input schema</label>" +
      "<pre style='background:rgba(0,0,0,.35);padding:10px;border-radius:8px;overflow:auto;max-height:220px;font-size:11px'>" +
      MD.escape(JSON.stringify(tool, null, 2)) + "</pre></div>" +
      "<div class='field'><label>Arguments (JSON)</label>" +
      "<textarea id='toolArgs'>{}</textarea>" +
      "<div class='hint'>studio_id is injected automatically when the tool requires it.</div></div>" +
      "<div id='toolResult'></div>",
      {
        actions: [
          { label: "Close", kind: "ghost", handler: closeModal },
          {
            label: "Run tool",
            kind: "primary",
            handler: async () => {
              let args = {};
              try {
                args = JSON.parse($("toolArgs").value || "{}");
              } catch (error) {
                toast("Arguments must be valid JSON", "error");
                return;
              }
              const box = $("toolResult");
              box.innerHTML = "<div class='status-line'>Running…</div>";
              try {
                const result = await call("mcp_call_tool", { name: tool.name, arguments: args });
                const text = (result.content || []).map((block) => block.text || "[" + (block.type || "block") + "]").join("\n");
                box.innerHTML = "<div class='status-line " + (result.isError ? "err" : "ok") + "'>" +
                  (result.isError ? "Tool reported an error" : "Completed") + "</div>" +
                  "<pre style='background:rgba(0,0,0,.35);padding:10px;border-radius:8px;overflow:auto;max-height:200px;font-size:11px;margin-top:8px'></pre>";
                box.querySelector("pre").textContent = text || "(no text content)";
              } catch (error) {
                box.innerHTML = "<div class='status-line err'></div>";
                box.querySelector(".status-line").textContent = errorText(error);
              }
            },
          },
        ],
      }
    );
  }

  // ------------------------------------------------------------------ settings

  function settingsDraft() {
    if (!S.settingsDraft) {
      S.settingsDraft = JSON.parse(JSON.stringify(S.settings || { ai: {}, mcp: {}, editor: {}, ui: {} }));
    }
    return S.settingsDraft;
  }

  // Switching settings tabs re-renders the panel, so read the visible fields
  // into the draft first; otherwise every unsaved edit is silently lost.
  function commitSettingsDraft() {
    const draft = S.settingsDraft;
    if (!draft) return;
    if ($("setBaseUrl")) {
      draft.ai.baseUrl = $("setBaseUrl").value.trim();
      draft.ai.model = $("setModel").value.trim();
      draft.ai.temperature = parseFloat($("setTemp").value) || 0.3;
      draft.ai.maxTokens = parseInt($("setMaxTokens").value, 10) || 8192;
      draft.ai.requestTimeoutSecs = parseInt($("setTimeout").value, 10) || 180;
      draft.ai.customInstructions = $("setInstructions").value;
    }
    if ($("setTransport")) {
      draft.mcp.transport = $("setTransport").value;
      draft.mcp.command = $("setCommand").value.trim();
      draft.mcp.args = $("setArgs").value.split(/\s+/).filter(Boolean);
      draft.mcp.httpUrl = $("setHttpUrl").value.trim();
      draft.mcp.studioId = $("setStudioId").value.trim();
      draft.mcp.autoConnect = $("tgAutoConnect").classList.contains("on");
      draft.mcp.autoReconnect = $("tgAutoReconnect").classList.contains("on");
      draft.mcp.confirmDestructive = $("tgConfirm").classList.contains("on");
      if ($("tgAutoApprove")) draft.mcp.autoApprove = $("tgAutoApprove").classList.contains("on");
    }
    if ($("setFontSize")) {
      draft.editor.fontSize = parseInt($("setFontSize").value, 10) || 13;
      draft.editor.tabSize = parseInt($("setTabSize").value, 10) || 4;
      draft.editor.autosaveSecs = parseInt($("setAutosave").value, 10) || 20;
      draft.editor.wordWrap = $("tgWordWrap").classList.contains("on");
    }
  }

  function openSettings(tab) {
    if (S.view === "settings" && S.settingsDraft) commitSettingsDraft();
    const current = settingsDraft();
    const active = tab || S.settingsTab || "ai";
    S.settingsTab = active;
    const tabButton = (id, label) =>
      "<button class='stab" + (id === active ? " is-active" : "") + "' data-stab='" + id + "'>" + label + "</button>";

    const ai = current.ai;
    const mcp = current.mcp;
    const editor = current.editor;

    const panels = {
      ai:
        "<div class='provider-card'><img src='assets/terrouter.svg' alt=''>" +
        "<div><div class='provider-name'>Ter Router</div><div class='provider-url'>" + MD.escape(ai.baseUrl) + "</div></div></div>" +
        "<div class='field'><label>Ter Router API key</label>" +
        "<input type='password' id='setApiKey' placeholder='" + (S.boot && S.boot.apiKeyMasked ? S.boot.apiKeyMasked : "tr_live_…") + "' autocomplete='off'>" +
        "<div class='hint'>Stored in the " + MD.escape(S.boot ? S.boot.secretBackend : "Windows Credential Manager") + ". Never written to settings or logs.</div></div>" +
        "<div style='display:flex;gap:8px;margin-bottom:14px'>" +
        "<button class='btn' id='saveKey'>Save key</button>" +
        "<button class='btn ghost' id='clearKey'>Remove</button>" +
        "<button class='btn' id='validateKey'>Validate connection</button></div>" +
        "<div id='keyStatus'></div>" +
        "<div class='field'><label>API base URL</label><input type='text' id='setBaseUrl' value='" + MD.escape(ai.baseUrl) + "'></div>" +
        "<div class='field'><label>Model</label><div style='display:flex;gap:8px'>" +
        "<input type='text' id='setModel' value='" + MD.escape(ai.model || "") + "' placeholder='anthropic/claude-sonnet-4.5'>" +
        "<button class='btn' id='loadModels'>Discover</button></div>" +
        "<div class='hint'>Discovered models appear in the top-bar selector.</div></div>" +
        "<div class='field-row-3'>" +
        "<div class='field'><label>Temperature</label><input type='number' id='setTemp' min='0' max='2' step='0.1' value='" + ai.temperature + "'></div>" +
        "<div class='field'><label>Max tokens</label><input type='number' id='setMaxTokens' min='256' step='256' value='" + ai.maxTokens + "'></div>" +
        "<div class='field'><label>Timeout (s)</label><input type='number' id='setTimeout' min='10' max='900' value='" + ai.requestTimeoutSecs + "'></div></div>" +
        "<div class='field'><label>Extra instructions</label><textarea id='setInstructions' placeholder='House rules for the assistant…'>" + MD.escape(ai.customInstructions || "") + "</textarea></div>",
      mcp:
        "<div class='field'><label>Transport</label><select id='setTransport'>" +
        "<option value='stdio'" + (mcp.transport === "stdio" ? " selected" : "") + ">stdio — built-in Studio MCP (recommended)</option>" +
        "<option value='http'" + (mcp.transport === "http" ? " selected" : "") + ">Streamable HTTP</option></select>" +
        "<div class='hint'>Roblox Studio ships an MCP server that TerCode launches as <code>cmd.exe /c %LOCALAPPDATA%\\Roblox\\mcp.bat</code>.</div></div>" +
        "<div class='field'><label>Custom command (optional)</label><input type='text' id='setCommand' value='" + MD.escape(mcp.command || "") + "' placeholder='leave empty to auto-detect'></div>" +
        "<div class='field'><label>Arguments</label><input type='text' id='setArgs' value='" + MD.escape((mcp.args || []).join(" ")) + "' placeholder='--stdio'></div>" +
        "<div class='field'><label>HTTP URL</label><input type='text' id='setHttpUrl' value='" + MD.escape(mcp.httpUrl || "") + "'></div>" +
        "<div class='field'><label>Studio id (optional)</label><input type='text' id='setStudioId' value='" + MD.escape(mcp.studioId || "") + "' placeholder='auto — first connected Studio'></div>" +
        "<div class='toggle'><div><div class='tg-label'>Connect automatically</div><div class='tg-hint'>Connect when TerCode starts.</div></div>" +
        "<div class='switch" + (mcp.autoConnect ? " on" : "") + "' id='tgAutoConnect'></div></div>" +
        "<div class='toggle'><div><div class='tg-label'>Reconnect automatically</div><div class='tg-hint'>Retry while Studio restarts.</div></div>" +
        "<div class='switch" + (mcp.autoReconnect ? " on" : "") + "' id='tgAutoReconnect'></div></div>" +
        "<div class='toggle'><div><div class='tg-label'>Confirm significant changes</div><div class='tg-hint'>Ask before script edits, saves and asset generation.</div></div>" +
        "<div class='switch" + (mcp.confirmDestructive ? " on" : "") + "' id='tgConfirm'></div></div>" +
        "<div class='toggle'><div><div class='tg-label'>Auto-approve tool calls</div><div class='tg-hint'>Run significant Studio calls without asking. A self-test always auto-approves its own run; leave this off for normal work.</div></div>" +
        "<div class='switch" + (mcp.autoApprove ? " on" : "") + "' id='tgAutoApprove'></div></div>" +
        "<div style='display:flex;gap:8px;margin-top:14px'>" +
        "<button class='btn' id='mcpReconnectNow'>Reconnect now</button>" +
        "<button class='btn ghost' id='mcpDiagnose'>Diagnostics</button></div>" +
        "<div id='mcpStatusBox' style='margin-top:10px'></div>",
      editor:
        "<div class='field-row-3'>" +
        "<div class='field'><label>Font size</label><input type='number' id='setFontSize' min='9' max='28' value='" + editor.fontSize + "'></div>" +
        "<div class='field'><label>Tab size</label><input type='number' id='setTabSize' min='2' max='8' value='" + editor.tabSize + "'></div>" +
        "<div class='field'><label>Autosave (s)</label><input type='number' id='setAutosave' min='5' max='600' value='" + editor.autosaveSecs + "'></div></div>" +
        "<div class='toggle'><div><div class='tg-label'>Word wrap</div><div class='tg-hint'>Wrap long lines in the editor.</div></div>" +
        "<div class='switch" + (editor.wordWrap ? " on" : "") + "' id='tgWordWrap'></div></div>",
      testing:
        "<p class='sub' style='margin-top:0'>The AI self-test scans every Luau file, then asks the selected model to " +
        "verify the place in Studio with tool calls auto-approved for that run. Playtest control and capture use the " +
        "Studio MCP tools <code>start_stop_play</code>, <code>get_studio_state</code>, <code>get_console_output</code> and " +
        "<code>screen_capture</code>. Nothing here is simulated: if your Studio build does not expose a " +
        "tool, TerCode says so instead of pretending it ran.</p>" +
        "<div class='settings-section'><h3>Verification</h3>" +
        "<p>Open the self-test page to see every static check and the assistant's live run.</p>" +
        "<button class='btn' id='openSelfTestFromSettings'><svg class='ic'><use href='#i-shield'/></svg>Open AI self-test</button></div>" +
        "<div class='field'><label>Studio session</label><div class='status-line' id='testingStatus'>checking…</div></div>" +
        "<div class='studio-actions'>" +
        "<button class='btn' data-play='start_play'>Start playtest</button>" +
        "<button class='btn' data-play='run_server'>Run server</button>" +
        "<button class='btn ghost' data-play='stop'>Stop</button></div>" +
        "<div class='studio-actions' style='margin-top:8px'>" +
        "<button class='btn ghost' id='testConsole'>Read console</button>" +
        "<button class='btn ghost' id='testShot'>Capture screenshot</button></div>" +
        "<pre class='studio-out' id='testOut' hidden></pre>" +
        "<div class='shot-strip' id='testShots'></div>",
      about:
        "<dl class='kv'>" +
        "<dt>Version</dt><dd>" + MD.escape(S.boot ? S.boot.version : "") + "</dd>" +
        "<dt>Data folder</dt><dd>" + MD.escape(S.boot ? S.boot.dataDir : "") + "</dd>" +
        "<dt>Credentials</dt><dd>" + MD.escape(S.boot ? S.boot.secretBackend : "") + "</dd>" +
        "<dt>Models cached</dt><dd>" + S.models.length + "</dd>" +
        "<dt>Studio</dt><dd>" + MD.escape(S.studioReport && S.studioReport.studioInstalled ? "installed" : "not found") + "</dd>" +
        "<dt>Rojo</dt><dd>" + MD.escape(S.boot && S.boot.rojo && S.boot.rojo.installed ? (S.boot.rojo.version || "installed") : "not installed") + "</dd>" +
        "</dl>" +
        "<div style='display:flex;gap:8px;margin-top:16px;flex-wrap:wrap'>" +
        "<button class='btn' id='checkUpdates'>Check for updates</button>" +
        "<button class='btn ghost' id='openRepo'>GitHub repository</button>" +
        "<button class='btn ghost' id='openTerRouter'>TerRouter.com</button></div>" +
        "<div id='updateStatus' style='margin-top:10px'></div>",
    };

    setView("settings");
    $("settingsTabs").innerHTML =
      tabButton("ai", "AI Provider") + tabButton("mcp", "Roblox MCP") +
      tabButton("testing", "Testing") + tabButton("editor", "Editor") + tabButton("about", "About");
    $("settingsPanel").innerHTML = panels[active] || panels.ai;
    $("settingsView").hidden = false;
    $("editorHost").hidden = true;
    $("bottomPanel").classList.add("collapsed");
    $("settingsSave").onclick = () => saveSettingsFromModal();
    $("settingsBack").onclick = closeSettings;
    document.querySelectorAll("#settingsTabs [data-stab]").forEach((button) => {
      button.addEventListener("click", () => openSettings(button.dataset.stab));
    });
    wireSettingsHandlers(active);
    if (active === "testing") wireTestingPanel();
  }

  function closeSettings() {
    S.settingsDraft = null;
    if (!S.settings || !S.settings.ui || S.settings.ui.showBottomPanel) {
      $("bottomPanel").classList.remove("collapsed");
    }
    setView(S.projectId ? "explorer" : "projects");
  }

  function wireTestingPanel() {
    $("settingsPanel").querySelectorAll("[data-play]").forEach((button) => {
      button.addEventListener("click", () => studioPlay(button.dataset.play));
    });
    $("testConsole").addEventListener("click", async () => {
      const out = $("testOut");
      out.hidden = false;
      out.textContent = "Reading the Studio console…";
      try {
        out.textContent = await call("studio_console");
      } catch (error) {
        out.textContent = errorText(error);
      }
    });
    $("testShot").addEventListener("click", () => studioScreenshot());
    const openTest = $("openSelfTestFromSettings");
    if (openTest) openTest.addEventListener("click", openSelfTest);
    renderShots($("testShots"));
    refreshPlayState();
  }

  function wireSettingsHandlers(active) {
    const saveKey = $("saveKey");
    if (saveKey) {
      saveKey.addEventListener("click", async () => {
        const key = $("setApiKey").value.trim();
        if (!key) { toast("Paste your API key first", "error"); return; }
        try {
          const masked = await call("set_api_key", { key });
          if (S.boot) S.boot.apiKeyMasked = masked;
          $("setApiKey").value = "";
          $("setApiKey").placeholder = masked || "stored";
          setKeyStatus("Key saved to " + (S.boot ? S.boot.secretBackend : "secure storage"), "ok");
          await refreshModels();
        } catch (error) {
          setKeyStatus(errorText(error), "err");
        }
      });
      $("clearKey").addEventListener("click", async () => {
        await call("clear_api_key").catch(() => {});
        if (S.boot) S.boot.apiKeyMasked = null;
        S.models = [];
        setKeyStatus("Key removed", "ok");
        renderModelLabel();
      });
      $("validateKey").addEventListener("click", async () => {
        setKeyStatus("Checking…", "");
        try {
          const report = await call("validate_api_key");
          setKeyStatus("Connected · " + report.modelCount + " models · " + report.latencyMs + " ms", "ok");
          await refreshModels();
        } catch (error) {
          setKeyStatus(errorText(error), "err");
        }
      });
      $("loadModels").addEventListener("click", async () => {
        try {
          const models = await call("list_models", { refresh: true });
          S.models = models;
          setKeyStatus(models.length + " models discovered — pick one in the top bar", "ok");
        } catch (error) {
          setKeyStatus(errorText(error), "err");
        }
      });
    }

    const transport = $("setTransport");
    if (transport) {
      ["tgAutoConnect", "tgAutoReconnect", "tgConfirm", "tgAutoApprove"].forEach((id) => {
        const node = $(id);
        if (!node) return;
        node.setAttribute("role", "switch");
        node.tabIndex = 0;
        const sync = () => node.setAttribute("aria-checked", node.classList.contains("on") ? "true" : "false");
        const toggle = () => {
          node.classList.toggle("on");
          sync();
        };
        sync();
        node.addEventListener("click", toggle);
        node.addEventListener("keydown", (event) => {
          if (event.key === " " || event.key === "Enter") {
            event.preventDefault();
            toggle();
          }
        });
      });
      $("mcpReconnectNow").addEventListener("click", async () => {
        const box = $("mcpStatusBox");
        box.innerHTML = "<div class='status-line'>Connecting…</div>";
        try {
          const status = await call("mcp_reconnect");
          S.mcpStatus = status;
          box.innerHTML = "<div class='status-line " + (status.state === "connected" ? "ok" : "err") + "'></div>";
          box.querySelector(".status-line").textContent = statusLabel(status);
          renderStatus();
        } catch (error) {
          box.innerHTML = "<div class='status-line err'></div>";
          box.querySelector(".status-line").textContent = errorText(error);
        }
      });
      $("mcpDiagnose").addEventListener("click", mcpDiagnosticsModal);
    }

    const wordWrap = $("tgWordWrap");
    if (wordWrap) wordWrap.addEventListener("click", () => wordWrap.classList.toggle("on"));

    const updates = $("checkUpdates");
    if (updates) {
      updates.addEventListener("click", async () => {
        const box = $("updateStatus");
        box.innerHTML = "<div class='status-line'>Checking GitHub releases…</div>";
        try {
          const result = await call("check_updates");
          box.innerHTML = "<div class='status-line " + (result.available ? "" : "ok") + "'></div>";
          box.querySelector(".status-line").textContent = result.available
            ? "Version " + result.latest + " is available (you have " + result.current + ")"
            : "You are on the latest release (" + result.current + ")";
          if (result.available) {
            const open = el("button", "btn primary", "Open release");
            open.style.marginTop = "8px";
            open.addEventListener("click", () => call("open_external", { url: result.url }));
            box.appendChild(open);
          }
        } catch (error) {
          box.innerHTML = "<div class='status-line err'></div>";
          box.querySelector(".status-line").textContent = errorText(error);
        }
      });
      $("openRepo").addEventListener("click", () =>
        call("open_external", { url: "https://github.com/itriedcoding/TerCode" }).catch(() => {}));
      $("openTerRouter").addEventListener("click", () =>
        call("open_external", { url: "https://terrouter.com" }).catch(() => {}));
    }
    if (active === "mcp") {
      const box = $("mcpStatusBox");
      if (box) {
        box.innerHTML = "<div class='status-line " + (S.mcpStatus.state === "connected" ? "ok" : "") + "'></div>";
        box.querySelector(".status-line").textContent = statusLabel(S.mcpStatus);
      }
    }
  }

  function setKeyStatus(message, kind) {
    const box = $("keyStatus");
    if (!box) return;
    box.innerHTML = "<div class='status-line " + (kind || "") + "'></div>";
    box.querySelector(".status-line").textContent = message;
  }

  async function saveSettingsFromModal() {
    commitSettingsDraft();
    const settings = JSON.parse(JSON.stringify(settingsDraft()));
    try {
      S.settings = await call("update_settings", { settings });
      S.settingsDraft = null;
      document.documentElement.style.setProperty("--assistant-w", S.settings.ui.assistantWidth + "px");
      applyEditorPrefs();
      updateAutoApproveChip();
      renderModelLabel();
      toast("Settings saved", "ok");
    } catch (error) {
      toast(errorText(error), "error");
    }
  }

  // -------------------------------------------------------------------- modal

  function openModal(html, options) {
    $("modalRoot").hidden = false;
    $("modal").innerHTML = html + '<div class="modal-actions" id="modalActions"></div>';
    const actions = $("modalActions");
    ((options && options.actions) || []).forEach((action) => {
      const button = el("button", "btn " + (action.kind || ""), action.label);
      button.addEventListener("click", action.handler);
      actions.appendChild(button);
    });
    $("modalRoot").querySelector(".modal-backdrop").onclick = closeModal;
  }

  function closeModal() {
    $("modalRoot").hidden = true;
    $("modal").innerHTML = "";
  }

  // ---------------------------------------------------------------- shortcuts

  function bindShortcuts() {
    document.addEventListener("keydown", (event) => {
      const mod = event.ctrlKey || event.metaKey;
      if (mod && event.key.toLowerCase() === "s") {
        event.preventDefault();
        saveActiveFile();
      } else if (mod && event.key === "Enter") {
        event.preventDefault();
        sendPrompt();
      } else if (mod && event.key === ",") {
        event.preventDefault();
        openSettings("ai");
      } else if (mod && event.key === "`") {
        event.preventDefault();
        toggleBottomPanel();
      } else if (mod && event.shiftKey && event.key.toLowerCase() === "r") {
        event.preventDefault();
        reconnectMcp();
      } else if (mod && event.key.toLowerCase() === "k") {
        event.preventDefault();
        openPalette();
      } else if (mod && event.shiftKey && event.key.toLowerCase() === "t") {
        event.preventDefault();
        openSelfTest();
        runSelfTest();
      } else if (mod && event.key.toLowerCase() === "p") {
        event.preventDefault();
        quickOpen();
      } else if (event.key === "Escape") {
        if (!$("modalRoot").hidden) closeModal();
        closeModelMenu();
      }
    });
  }

  function quickOpen() {
    if (!S.files.length) { toast("Open a project to quick-open files", "error"); return; }
    openModal(
      "<h2>Quick open</h2><p class='sub'>Jump to a file in this project.</p>" +
      "<div class='input-row'><svg class='ic'><use href='#i-search'/></svg><input id='qoInput' placeholder='Type a file name…' autocomplete='off'></div>" +
      "<div id='qoList' style='max-height:320px;overflow:auto'></div>",
      { actions: [{ label: "Close", kind: "ghost", handler: closeModal }] }
    );
    const render = (query) => {
      const list = $("qoList");
      list.innerHTML = "";
      S.files
        .filter((file) => !query || file.toLowerCase().includes(query.toLowerCase()))
        .slice(0, 40)
        .forEach((file) => {
          const row = el("button", "prompt-history-item", MD.escape(file));
          row.addEventListener("click", () => { closeModal(); openFile(file); });
          list.appendChild(row);
        });
    };
    render("");
    $("qoInput").addEventListener("input", (event) => render(event.target.value));
    $("qoInput").focus();
  }

  // Bottom tab events
  document.querySelectorAll(".btab").forEach((tab) => {
    tab.addEventListener("click", () => setBottomTab(tab.dataset.panel));
  });

  document.addEventListener("DOMContentLoaded", start);
  if (document.readyState !== "loading") start();
})();
