/* TerCode — syntax-highlighted editor.
   A transparent textarea sits over a highlighted <pre>, so editing uses native
   input (IME, undo, selection) while the painted layer shows token colours. */
(function () {
  "use strict";

  const KEYWORDS = new Set([
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function",
    "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true",
    "until", "while", "continue", "type", "export",
  ]);

  const GLOBALS = new Set([
    "game", "workspace", "script", "shared", "plugin", "print", "warn", "error",
    "require", "typeof", "ipairs", "pairs", "next", "pcall", "xpcall", "select",
    "tostring", "tonumber", "assert", "unpack", "setmetatable", "getmetatable",
    "rawget", "rawset", "task", "os", "math", "string", "table", "coroutine",
    "utf8", "buffer", "bit32", "Instance", "Vector2", "Vector3", "CFrame",
    "UDim", "UDim2", "Color3", "ColorSequence", "BrickColor", "Rect", "Ray",
    "Region3", "NumberRange", "NumberSequence", "TweenInfo", "Enum", "DateTime",
    "Random", "Faces", "Axes", "RaycastParams", "OverlapParams", "PhysicalProperties",
    "Font", "Content", "PathWaypoint", "DockWidgetPluginGuiInfo",
  ]);

  function esc(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  function span(cls, text) {
    return '<span class="' + cls + '">' + esc(text) + "</span>";
  }

  function highlightLuau(code) {
    let out = "";
    let i = 0;
    const n = code.length;

    while (i < n) {
      const c = code[i];

      // Comments: --[[ long ]] and -- line
      if (c === "-" && code[i + 1] === "-") {
        const longStart = code.slice(i).match(/^--\[(=*)\[/);
        if (longStart) {
          const close = "]" + longStart[1] + "]";
          const end = code.indexOf(close, i + longStart[0].length);
          const stop = end === -1 ? n : end + close.length;
          out += span("tok-com", code.slice(i, stop));
          i = stop;
          continue;
        }
        let end = code.indexOf("\n", i);
        if (end === -1) end = n;
        out += span("tok-com", code.slice(i, end));
        i = end;
        continue;
      }

      // Long strings [[ ]]
      const longStr = code.slice(i).match(/^\[(=*)\[/);
      if (longStr) {
        const close = "]" + longStr[1] + "]";
        const end = code.indexOf(close, i + longStr[0].length);
        const stop = end === -1 ? n : end + close.length;
        out += span("tok-str", code.slice(i, stop));
        i = stop;
        continue;
      }

      // Quoted strings
      if (c === '"' || c === "'" || c === "`") {
        let j = i + 1;
        while (j < n) {
          if (code[j] === "\\") { j += 2; continue; }
          if (code[j] === c) { j += 1; break; }
          if (code[j] === "\n" && c !== "`") break;
          j += 1;
        }
        out += span("tok-str", code.slice(i, j));
        i = j;
        continue;
      }

      // Numbers
      if (/[0-9]/.test(c)) {
        let j = i;
        while (j < n && /[0-9a-zA-Z._xX]/.test(code[j])) j += 1;
        out += span("tok-num", code.slice(i, j));
        i = j;
        continue;
      }

      // Identifiers / keywords
      if (/[A-Za-z_]/.test(c)) {
        let j = i;
        while (j < n && /[0-9A-Za-z_]/.test(code[j])) j += 1;
        const word = code.slice(i, j);
        let k = j;
        while (k < n && (code[k] === " " || code[k] === "\t")) k += 1;
        if (KEYWORDS.has(word)) {
          out += span("tok-kw", word);
        } else if (GLOBALS.has(word)) {
          out += span("tok-glob", word);
        } else if (code[k] === "(") {
          out += span("tok-fn", word);
        } else if (code[j] === ":" || (code[k - 1] === ":" ) || /^\s*:/.test(code.slice(j, j + 2))) {
          out += span("tok-prop", word);
        } else {
          out += esc(word);
        }
        i = j;
        continue;
      }

      if ("+-*/%^#=~<>(){}[];:,.".indexOf(c) !== -1) {
        out += span("tok-op", c);
        i += 1;
        continue;
      }

      out += esc(c);
      i += 1;
    }
    return out;
  }

  function highlightJson(code) {
    let out = "";
    let i = 0;
    const n = code.length;
    while (i < n) {
      const c = code[i];
      if (c === '"') {
        let j = i + 1;
        while (j < n) {
          if (code[j] === "\\") { j += 2; continue; }
          if (code[j] === '"') { j += 1; break; }
          j += 1;
        }
        const text = code.slice(i, j);
        let k = j;
        while (k < n && /\s/.test(code[k])) k += 1;
        out += span(code[k] === ":" ? "tok-json-key" : "tok-json-str", text);
        i = j;
        continue;
      }
      if (/[0-9-]/.test(c) && /[0-9]/.test(code[i + 1] || "")) {
        let j = i + 1;
        while (j < n && /[0-9.eE+-]/.test(code[j])) j += 1;
        out += span("tok-num", code.slice(i, j));
        i = j;
        continue;
      }
      if (/[a-z]/.test(c)) {
        let j = i;
        while (j < n && /[a-z]/.test(code[j])) j += 1;
        const word = code.slice(i, j);
        if (word === "true" || word === "false" || word === "null") out += span("tok-kw", word);
        else out += esc(word);
        i = j;
        continue;
      }
      out += esc(c);
      i += 1;
    }
    return out;
  }

  function highlight(code, path) {
    const lower = (path || "").toLowerCase();
    if (lower.endsWith(".json")) return highlightJson(code);
    if (lower.endsWith(".luau") || lower.endsWith(".lua") || lower.endsWith(".rbxmx") || lower.endsWith(".rbxlx")) {
      return highlightLuau(code);
    }
    return esc(code);
  }

  function create(options) {
    const textarea = options.textarea;
    const pre = options.highlight;
    const codeEl = options.highlightCode;
    const gutter = options.gutterInner;
    let path = "";
    let onChange = function () {};
    let lastLines = -1;

    function paint() {
      const value = textarea.value;
      codeEl.innerHTML = highlight(value, path) + "\n";
      const lines = value.split("\n").length;
      if (lines !== lastLines) {
        lastLines = lines;
        let html = "";
        for (let i = 1; i <= lines; i += 1) html += "<div>" + i + "</div>";
        gutter.innerHTML = html;
      }
      syncScroll();
      onChange(value);
    }

    function syncScroll() {
      pre.scrollTop = textarea.scrollTop;
      pre.scrollLeft = textarea.scrollLeft;
      gutter.style.transform = "translateY(" + -textarea.scrollTop + "px)";
    }

    textarea.addEventListener("input", paint);
    textarea.addEventListener("scroll", syncScroll, { passive: true });
    textarea.addEventListener("keydown", function (event) {
      if (event.key === "Tab") {
        event.preventDefault();
        const tabSize = options.tabSize ? options.tabSize() : 4;
        const indent = " ".repeat(tabSize);
        const start = textarea.selectionStart;
        const end = textarea.selectionEnd;
        textarea.setRangeText(indent, start, end, "end");
        paint();
      }
    });

    return {
      setContent(value, filePath) {
        path = filePath || "";
        textarea.value = value;
        lastLines = -1;
        paint();
        textarea.scrollTop = 0;
        textarea.scrollLeft = 0;
        syncScroll();
      },
      getContent() { return textarea.value; },
      setPath(filePath) { path = filePath || ""; paint(); },
      refresh: paint,
      focus() { textarea.focus(); },
      markDiagnostics(diagnostics) {
        const byLine = {};
        (diagnostics || []).forEach(function (d) {
          const current = byLine[d.line];
          const severity = d.severity;
          if (current === "error") return;
          if (!current || severity === "error") byLine[d.line] = severity;
        });
        Array.from(gutter.children).forEach(function (child, index) {
          const severity = byLine[index + 1];
          child.className = severity === "error" ? "err" : severity === "warning" ? "warn" : "";
        });
      },
      onChange(cb) { onChange = cb; },
    };
  }

  window.Editor = { create: create, highlight: highlight };
})();
