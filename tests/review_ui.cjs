// node tests/review_ui.cjs: RE8 to RE12. The operators' Reports panel shows only while god mode is
// on, lists the queue minor_safety first with each group's copy, reasons and reporters as text,
// closes a group with an action after a confirm, and lifts holds and releases kept copies the same
// way. It runs the shipped panel code from web/app.js against a fake DOM.
const fs = require('node:fs'), vm = require('node:vm'), assert = require('node:assert/strict');
const app = fs.readFileSync('web/app.js', 'utf8').replace(/\r\n/g, '\n');
class Node {
  constructor(tag, props = {}, children = []) {
    this.tag = tag; this.props = { ...props }; this.children = [];
    Object.assign(this, props); this.append(...children);
  }
  append(...c) { this.children.push(...c.flat(Infinity).filter((x) => x != null && x !== false)); }
  replaceChildren(...c) { this.children = []; this.append(...c); }
  // Report text must never be parsed as markup.
  set innerHTML(_) { throw new Error('innerHTML used'); }
}
const all = (n) => (n instanceof Node ? [n, ...n.children.flatMap(all)] : []);
const text = (n) => (n instanceof Node ? n.children.map(text).join('') : String(n));
const h = (tag, props, ...children) => new Node(tag, props, children);
const tick = () => new Promise((r) => setImmediate(r));
const slice = (from, to) => {
  const start = app.indexOf(from);
  assert.ok(start >= 0, `web/app.js has ${from}`);
  return app.slice(start, app.indexOf(to, start) + to.length);
};

const NASTY = '<img src=x onerror=alert(1)>';
function queue() {
  return {
    groups: [
      {
        report_id: 9, target_kind: 'message', target_id: '41', minor_safety: true,
        reasons: ['minor_safety'], reporters: 1, by_bot: 0,
        copy: { speaker: 'Bo', text: 'asked for photos', room: 'Ann\'s room', said_at: '2026-11-06T10:00:00Z' },
        room: { id: 'r1', name: 'Ann\'s room' }, person: { id: 'p2', name: 'Bo' }, bot: null,
        oldest_at: 1_800_000_100,
        reports: [{ id: 9, reason: 'minor_safety', details: null, by_bot: false, reporter: { id: 'p1', name: 'Ann' }, bot: null, created_at: 1_800_000_100 }],
      },
      {
        report_id: 7, target_kind: 'message', target_id: '40', minor_safety: false,
        reasons: ['harassment', 'hate'], reporters: 2, by_bot: 1,
        copy: { speaker: 'Bo', text: NASTY, speaker_bot_id: null },
        room: { id: 'r1', name: 'Ann\'s room' }, person: { id: 'p2', name: 'Bo' }, bot: { id: 'b1', name: 'Pip' },
        oldest_at: 1_800_000_000,
        reports: [
          { id: 7, reason: 'harassment', details: 'twice', by_bot: false, reporter: { id: 'p1', name: 'Ann' }, bot: null, created_at: 1_800_000_000 },
          { id: 8, reason: 'hate', details: null, by_bot: true, reporter: { id: 'p3', name: 'Cy' }, bot: { id: 'b2', name: 'Scout' }, created_at: 1_800_000_050 },
        ],
      },
    ],
    holds: [{ hold: 'suspend_bot', id: 'b1', name: 'Pip', person: { id: 'p2', name: 'Bo' }, reason: 'spam', report_id: 3, at: 1_800_000_000 }],
    held: [{ report_id: 2, closed_at: 1_790_000_000 }],
  };
}

function load() {
  const calls = [], asked = [];
  let answer = true, refuse = null;
  const api = async (path, options = {}) => {
    calls.push({ path, method: options.method || 'GET', body: options.body });
    if (refuse) { const e = new Error(refuse); e.status = 403; throw e; }
    return options.method === 'POST' ? { ok: true } : queue();
  };
  const panels = {
    current: null, added: {},
    add(name, title, options) { const body = h('div', {}); this.added[name] = { title, options, body }; return body; },
    open(name) { this.current = name; this.added[name].options.onOpen?.(); },
    close() { this.current = null; },
  };
  const context = { h, api, panels, confirm: (q) => { asked.push(q); return answer; } };
  vm.createContext(context);
  vm.runInContext([
    slice('function formError() {', '\n}\n'),
    slice('function field(label, input, hint) {', '\n}\n'),
    slice('function act(errorEl, run) {', '\n}\n'),
    slice('const dayFormat = ', ';\n'),
    slice('const timeFormat = ', ';\n'),
    slice('const fromUnix = ', ';\n'),
    slice('const whenText = ', ';\n'),
    slice('const REPORT_ACTIONS = [', '];\n'),
    slice('const HOLD_NAMES = ', ';\n'),
    slice('function reportsPanel(panels) {', '\n}\n'),
    'this.reportsPanel = reportsPanel; this.whenText = whenText;',
  ].join('\n'), context);
  return { context, calls, asked, panels, say: (yes) => { answer = yes; }, refuse: (why) => { refuse = why; } };
}
const button = (root, label) => all(root).find((n) => n.tag === 'button' && text(n) === label);
const buttons = (root, label) => all(root).filter((n) => n.tag === 'button' && text(n) === label);

(async () => {
  const { context, calls, asked, panels, say, refuse } = load();
  const reports = context.reportsPanel(panels);
  const body = panels.added.reports.body;
  assert.equal(panels.added.reports.title, 'Reports');

  // Hidden until god mode is on, and nothing is asked before it opens.
  assert.equal(reports.row.hidden, true);
  assert.equal(calls.length, 0);
  reports.show(true);
  assert.equal(reports.row.hidden, false);
  reports.row.onclick();
  await tick();
  assert.equal(panels.current, 'reports');
  assert.deepEqual(calls.map((c) => `${c.method} ${c.path}`), ['GET /api/reports']);

  // RE8: minor_safety first, marked so, with its copy, the room and the person.
  const groups = all(body).filter((n) => n.tag === 'article');
  assert.equal(groups.length, 2);
  assert.match(text(groups[0]), /^Minor safety\. minor_safety about a message, reported by 1/);
  assert.ok(groups[0].props.class.includes('report-urgent'));
  assert.match(text(groups[0]), /Person: Bo · Room: Ann's room/);
  assert.match(text(groups[0]), /text: asked for photos/);
  // RE9: the reporters by name, a bot's report shown as one, and the details.
  const second = text(groups[1]);
  assert.match(second, /harassment, hate about a message, reported by 2/);
  assert.ok(second.includes('Ann (harassment, ') && second.includes('): twice'), second);
  assert.ok(second.includes('Cy, through their bot Scout (hate, '), second);
  // Report text goes in as a text child, never parsed.
  const copyLine = all(groups[1]).find((n) => n.tag === 'li' && text(n).startsWith('text: '));
  assert.deepEqual(copyLine.children, [`text: ${NASTY}`]);
  assert.ok(!slice('function reportsPanel(panels) {', '\n}\n').includes('innerHTML'));
  // Every action is offered.
  const select = all(groups[1]).find((n) => n.tag === 'select');
  assert.deepEqual(select.children.map((o) => o.value),
    ['dismiss', 'remove_content', 'warn', 'suspend_bot', 'restrict_person', 'ban_person']);

  // RE10: closing asks first, and a no sends nothing.
  select.value = 'warn';
  all(groups[1]).find((n) => n.tag === 'textarea').value = 'first time';
  say(false);
  await button(groups[1], 'Close report').onclick({ currentTarget: {} });
  assert.equal(calls.length, 1, 'no call without a yes');
  assert.match(asked[0], /^Warn the person\? This closes all 2 reports about it/);
  say(true);
  await button(groups[1], 'Close report').onclick({ currentTarget: {} });
  await tick();
  assert.equal(calls[1].path, '/api/reports/7/close');
  assert.equal(calls[1].method, 'POST');
  assert.equal(JSON.stringify(calls[1].body), JSON.stringify({ action: 'warn', note: 'first time' }));
  assert.equal(calls[2].path, '/api/reports', 'and the queue is read again');

  // RE12: holds can be lifted, after a confirm.
  assert.match(text(body), /Bot suspended: Pip, Bo's bot \(spam, /);
  await button(body, 'Lift').onclick({ currentTarget: {} });
  await tick();
  assert.match(asked.at(-1), /^Lift this for Pip\?/);
  const lifted = calls.find((c) => c.path === '/api/reports/lift');
  assert.equal(JSON.stringify(lifted.body), JSON.stringify({ hold: 'suspend_bot', id: 'b1' }));

  // RE16: a kept minor_safety copy can be released.
  assert.match(text(body), /Minor-safety copies kept/);
  await button(body, 'Release').onclick({ currentTarget: {} });
  await tick();
  assert.match(asked.at(-1), /^Release report 2\?/);
  assert.ok(calls.some((c) => c.path === '/api/reports/2/release' && c.method === 'POST'));

  // A refusal is shown in the server's words, beside the form.
  refuse('turn god mode on first');
  const now = all(body).filter((n) => n.tag === 'article');
  // A real select starts on its first option.
  all(now[0]).find((n) => n.tag === 'select').value = 'dismiss';
  await buttons(now[0], 'Close report')[0].onclick({ currentTarget: {} });
  const error = all(now[0]).find((n) => n.props.class === 'error');
  assert.equal(error.hidden, false);
  assert.equal(error.textContent, 'turn god mode on first');

  // Turning god mode off hides it and closes it.
  reports.show(false);
  assert.equal(reports.row.hidden, true);
  assert.equal(panels.current, null);

  // An empty queue says so.
  const empty = load();
  const api = empty.context.api;
  empty.context.api = async (path, options) => (path === '/api/reports' && !options ? { groups: [], holds: [], held: [] } : api(path, options));
  vm.runInContext('this.reportsPanel = reportsPanel;', empty.context);
  const blank = empty.context.reportsPanel(empty.panels);
  blank.show(true);
  blank.row.onclick();
  await tick();
  const said = text(empty.panels.added.reports.body);
  assert.match(said, /No open reports\./);
  assert.match(said, /Nobody\./);

  // On the map it's made only for someone the god list names, beside bot sign-ups.
  assert.ok(app.includes("const reports = typeof me?.god_mode === 'boolean' ? reportsPanel(panels) : null;"));
  assert.ok(app.includes('reports?.show(godOn);'));
  assert.ok(app.includes('    reports?.row,\n'));
  console.log('reports panel: ok');
})().catch((e) => { console.error(e); process.exit(1); });

// RE19: Download my data, on the account card and the suspended screen. Clicking it fetches the
// export and saves it as a file, and a refusal, such as the hourly limit, is said in the server's
// words. It runs the shipped link and screen from web/app.js.
(async () => {
  const fetched = [], saved = [], revoked = [];
  let reply = { ok: true, status: 200, blob: async () => 'the file' };
  const body = h('body', {});
  const shown = { screen: null };
  const context = {
    h,
    fetch: async (path, options) => { fetched.push({ path, options }); return reply; },
    URL: { createObjectURL: (blob) => { saved.push(blob); return 'blob:data'; }, revokeObjectURL: (url) => revoked.push(url) },
    document: { body },
    setTimeout: (run) => run(),
    api: async (path) => (path === '/api/me' ? { name: 'Cy', notices: [{ kind: 'ban_person', text: 'An operator suspended your account.' }] } : {}),
    app: { replaceChildren: (screen) => { shown.screen = screen; } },
    deleteAccountDialog: () => {},
    go: async () => {},
  };
  // A saved link clicks itself away.
  Node.prototype.click = function click() { this.clicked = true; };
  Node.prototype.remove = function remove() { body.children = body.children.filter((n) => n !== this); };
  vm.createContext(context);
  vm.runInContext([
    slice('function myDataLink() {', '\n}\n'),
    slice('async function showBanned() {', '\n}\n'),
    'this.myDataLink = myDataLink; this.showBanned = showBanned;',
  ].join('\n'), context);

  const row = context.myDataLink();
  const link = all(row).find((n) => n.tag === 'a');
  assert.equal(text(link), 'Download my data');
  assert.equal(link.href, '/api/account/export');
  let prevented = false;
  await link.onclick({ preventDefault: () => { prevented = true; } });
  assert.ok(prevented, 'the page fetches it rather than following the link');
  assert.equal(fetched[0].path, '/api/account/export');
  assert.equal(fetched[0].options.credentials, 'same-origin');
  assert.deepEqual(saved, ['the file']);
  assert.deepEqual(revoked, ['blob:data']);
  assert.equal(body.children.length, 0, 'the saving link is gone again');
  assert.match(text(row), /Downloaded\./);

  // The hourly limit, in the server's words.
  reply = { ok: false, status: 429, json: async () => ({ error: 'you can download your data once an hour: try again later' }) };
  await link.onclick({ preventDefault: () => {} });
  assert.match(text(row), /once an hour/);
  assert.equal(saved.length, 1, 'nothing more was saved');

  // The suspended screen has it, beside Delete account.
  await context.showBanned();
  const screen = all(shown.screen);
  assert.ok(screen.some((n) => n.tag === 'a' && text(n) === 'Download my data'), 'the suspended screen has the link');
  assert.ok(screen.some((n) => n.tag === 'button' && text(n) === 'Delete account'));
  // And so does the account card, beside Sign out.
  assert.ok(app.includes("    ownerKeyControl(me),\n    myDataLink(),\n"));
  console.log('download my data: ok');
})().catch((e) => { console.error(e); process.exit(1); });
