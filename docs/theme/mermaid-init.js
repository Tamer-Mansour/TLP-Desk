// Render ```mermaid code blocks as diagrams in the published book.
//
// mdBook emits them as <pre><code class="language-mermaid">. GitHub renders the same blocks
// natively, so the markdown stays readable in both places. This script only runs on the
// documentation website; the TLP-Desk application itself never loads anything from the internet.
(function () {
  var blocks = document.querySelectorAll('code.language-mermaid');
  if (blocks.length === 0) return;
  blocks.forEach(function (code) {
    var div = document.createElement('div');
    div.className = 'mermaid';
    div.textContent = code.textContent;
    code.parentElement.replaceWith(div);
  });
  var dark = /(coal|navy|ayu)/.test(document.documentElement.className);
  import('https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.esm.min.mjs').then(function (m) {
    m.default.initialize({ startOnLoad: false, theme: dark ? 'dark' : 'default' });
    m.default.run({ querySelector: '.mermaid' });
  });
})();
