/* TerCode — minimal, dependency-free Markdown renderer.
   Everything is HTML-escaped before any formatting so model output can never
   inject markup. Fenced code blocks get copy / insert actions. */
(function () {
  "use strict";

  function escapeHtml(text) {
    return String(text)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function inline(text) {
    let out = escapeHtml(text);
    // `code`
    out = out.replace(/`([^`\n]+)`/g, "<code>$1</code>");
    // **bold**
    out = out.replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>");
    // *italic*
    out = out.replace(/(^|[^*])\*([^*\n]+)\*(?!\*)/g, "$1<em>$2</em>");
    // [label](https://…)
    out = out.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,
      '<a href="$2" data-external="1" rel="noreferrer">$1</a>');
    return out;
  }

  function codeBlock(lang, code) {
    const language = (lang || "").toLowerCase();
    const body = escapeHtml(code);
    return (
      '<div class="code-block" data-lang="' + escapeHtml(language) + '">' +
      '<div class="cb-actions">' +
      '<button class="cb-btn" data-action="copy-code">Copy</button>' +
      '<button class="cb-btn" data-action="insert-code">Insert</button>' +
      "</div>" +
      "<pre><code>" + body + "</code></pre>" +
      "</div>"
    );
  }

  function renderBlocks(lines) {
    const out = [];
    let list = null;
    let paragraph = [];

    function flushParagraph() {
      if (paragraph.length) {
        out.push("<p>" + inline(paragraph.join(" ")) + "</p>");
        paragraph = [];
      }
    }
    function flushList() {
      if (list) {
        out.push("<" + list.tag + ">" +
          list.items.map(function (i) { return "<li>" + inline(i) + "</li>"; }).join("") +
          "</" + list.tag + ">");
        list = null;
      }
    }

    lines.forEach(function (raw) {
      const line = raw.replace(/\s+$/, "");
      if (!line.trim()) {
        flushParagraph();
        flushList();
        return;
      }
      const heading = line.match(/^(#{1,4})\s+(.*)$/);
      if (heading) {
        flushParagraph();
        flushList();
        const level = Math.min(heading[1].length + 2, 6);
        out.push("<h" + level + ">" + inline(heading[2]) + "</h" + level + ">");
        return;
      }
      const bullet = line.match(/^\s*[-*+]\s+(.*)$/);
      if (bullet) {
        flushParagraph();
        if (!list || list.tag !== "ul") { flushList(); list = { tag: "ul", items: [] }; }
        list.items.push(bullet[1]);
        return;
      }
      const numbered = line.match(/^\s*\d+[.)]\s+(.*)$/);
      if (numbered) {
        flushParagraph();
        if (!list || list.tag !== "ol") { flushList(); list = { tag: "ol", items: [] }; }
        list.items.push(numbered[1]);
        return;
      }
      flushList();
      paragraph.push(line.trim());
    });

    flushParagraph();
    flushList();
    return out.join("");
  }

  function render(source) {
    const text = String(source || "");
    const parts = [];
    const buffer = [];
    let inCode = false;
    let lang = "";
    let codeLines = [];

    function flushBuffer() {
      if (buffer.length) {
        parts.push(renderBlocks(buffer.slice()));
        buffer.length = 0;
      }
    }

    text.split("\n").forEach(function (line) {
      const fence = line.match(/^\s*```\s*([\w+-]*)\s*$/);
      if (fence) {
        if (inCode) {
          parts.push(codeBlock(lang, codeLines.join("\n")));
          inCode = false;
          lang = "";
          codeLines = [];
        } else {
          flushBuffer();
          inCode = true;
          lang = fence[1] || "";
        }
        return;
      }
      if (inCode) {
        codeLines.push(line);
      } else {
        buffer.push(line);
      }
    });

    if (inCode) parts.push(codeBlock(lang, codeLines.join("\n")));
    flushBuffer();
    return parts.join("") || '<p class="muted"></p>';
  }

  window.MD = { render: render, escape: escapeHtml };
})();
