// Standalone preview runtime for the exported Alkahest design screens.
//
// The canvas sources in ../source/*.dc.html run inside the Design canvas
// runtime. This file is a minimal replacement so the exported copies in this
// folder open directly in a browser (file:// works, no build step):
//
//   - reads the template from <script type="text/x-dc-template">,
//   - instantiates `class Component extends DCLogic` with the data-props
//     defaults and calls renderVals(),
//   - expands {{ path }} holes, <sc-for list as> and <sc-if value> blocks,
//   - moves <helmet> content into <head>.
//
// Event handlers, state updates and <dc-import> are not supported; the
// screens do not use them.
(function () {
  'use strict';

  class DCLogic {
    constructor(props) {
      this.props = props || {};
      this.state = {};
    }
    setState(patch) {
      Object.assign(this.state, typeof patch === 'function' ? patch(this.state) : patch);
    }
    forceUpdate() {}
  }

  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  // Resolves a dotted path or a literal inside a hole.
  function lookup(expr, scope) {
    const path = expr.trim();
    if (path === 'true') return true;
    if (path === 'false') return false;
    if (path === 'null') return null;
    if (/^-?\d+(\.\d+)?$/.test(path)) return Number(path);
    let value = scope;
    for (const part of path.split('.')) {
      if (value == null) return undefined;
      value = value[part];
    }
    return value;
  }

  function attr(tag, name) {
    const m = tag.match(new RegExp(name + '="([^"]*)"'));
    return m ? m[1] : null;
  }

  function holeValue(raw, scope) {
    const m = raw && raw.match(/^\s*\{\{([^}]*)\}\}\s*$/);
    return m ? lookup(m[1], scope) : raw;
  }

  // Finds the index just after the closing tag that matches the block opened at `start`.
  function findClose(tpl, start, name) {
    const open = '<' + name;
    const close = '</' + name + '>';
    let depth = 0;
    let i = start;
    while (i < tpl.length) {
      const nextOpen = tpl.indexOf(open, i);
      const nextClose = tpl.indexOf(close, i);
      if (nextClose === -1) throw new Error('unclosed <' + name + '>');
      if (nextOpen !== -1 && nextOpen < nextClose) {
        depth++;
        i = nextOpen + open.length;
      } else {
        depth--;
        i = nextClose + close.length;
        if (depth === 0) return { innerEnd: nextClose, end: i };
      }
    }
    throw new Error('unclosed <' + name + '>');
  }

  // Single left-to-right pass, so substituted values are never re-scanned.
  function render(tpl, scope) {
    let out = '';
    let i = 0;
    while (i < tpl.length) {
      const block = tpl.startsWith('<sc-for', i) ? 'sc-for' : tpl.startsWith('<sc-if', i) ? 'sc-if' : null;
      if (block) {
        const tagEnd = tpl.indexOf('>', i) + 1;
        const tag = tpl.slice(i, tagEnd);
        const { innerEnd, end } = findClose(tpl, i, block);
        const inner = tpl.slice(tagEnd, innerEnd);
        if (block === 'sc-for') {
          const list = holeValue(attr(tag, 'list'), scope) || [];
          const as = attr(tag, 'as') || 'item';
          list.forEach((item, index) => {
            out += render(inner, Object.assign({}, scope, { [as]: item, $index: index }));
          });
        } else if (holeValue(attr(tag, 'value'), scope)) {
          out += render(inner, scope);
        }
        i = end;
        continue;
      }
      if (tpl.startsWith('{{', i)) {
        const close = tpl.indexOf('}}', i);
        const value = lookup(tpl.slice(i + 2, close), scope);
        out += value == null ? '' : escapeHtml(value);
        i = close + 2;
        continue;
      }
      out += tpl[i];
      i++;
    }
    // Boolean attributes bound to holes render as "true"/"false".
    return out.replace(/\s(checked|disabled|selected)="(true|false)"/g, (_, name, v) => (v === 'true' ? ' ' + name : ''));
  }

  function mount() {
    const tplNode = document.querySelector('script[type="text/x-dc-template"]');
    const logicNode = document.querySelector('script[data-dc-script]');
    if (!tplNode || !logicNode) return;

    const decl = JSON.parse(logicNode.getAttribute('data-props') || '{}');
    const props = {};
    for (const [key, spec] of Object.entries(decl)) {
      if (!key.startsWith('$') && spec && 'default' in spec) props[key] = spec.default;
    }
    const Component = new Function('DCLogic', logicNode.textContent + '\nreturn Component;')(DCLogic);
    const vals = new Component(props).renderVals();

    let html = render(tplNode.textContent, vals);
    html = html.replace(/<helmet>([\s\S]*?)<\/helmet>/, (_, head) => {
      document.head.insertAdjacentHTML('beforeend', head);
      return '';
    });
    const host = document.createElement('div');
    host.innerHTML = html;
    tplNode.replaceWith(host);
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount);
  else mount();
})();
