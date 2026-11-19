const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const src = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
function section(a, b) {
  const start = src.indexOf(a), end = src.indexOf(b, start);
  assert(start >= 0 && end > start, `missing source section ${a}`);
  return src.slice(start, end);
}
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
function setup() {
  const saved = [{ id: 1, name: 'A', tone: 'pass', strength: .65, bw: false, position: 0 },
    { id: 2, name: 'B', tone: 'pass', strength: .65, bw: false, position: 1 }];
  const pending = new Map(), calls = [], elements = new Map(), timers = new Map();
  let timerId = 0;
  function el(id) {
    if (!elements.has(id)) elements.set(id, { hidden: false, value: '', innerHTML: '', textContent: '', style: {}, dataset: {}, handlers: {},
      classList: { add() {}, remove() {} }, addEventListener(type, fn) { this.handlers[type] = fn; }, focus() {},
      setAttribute(name, value) { (this.attributes ||= {})[name] = value; },
      showModal() { this.open = true; }, close(value) { this.open = false; this.returnValue = value; this.handlers.close?.(); } });
    return elements.get(id);
  }
  const ctx = { state: { luts: structuredClone(saved), sel: { kind: 'local', id: 1, num: 1 }, dirty: new Set(),
      cards: [{ mount_point: '/card-A', volume_name: 'A', device: 'disk1' }], cardMount: '/card-A', tf: { items: [], changed: {} } },
    writeBusy: false, cardQueue: Promise.resolve(), pv: {}, document: { hidden: false, querySelector: () => el('app'), querySelectorAll: () => [], activeElement: null },
    $: el, setTimeout(fn) { const n = ++timerId; timers.set(n, fn); return n; }, clearTimeout(n) { timers.delete(n); },
    renderDirty() {}, renderCard() {}, renderLib() {}, renderTf() {}, renderRing() {}, syncEditor() {}, requestPreview() {},
    renderCardStatus() {}, hideBanner() {}, showBanner() {}, log() {}, setStatus() {}, toast() {}, esc: String, loadCards: async () => {},
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (ctx.hook) { const result = await ctx.hook(command, args); if (result !== undefined) return result; }
      if (command === 'edit_lut') { pending.set(args.id, { ...args }); return { hash: String(args.strength) }; }
      if (command === 'flush_edits') {
        const ids = [...pending.keys()];
        pending.forEach((edit, id) => Object.assign(saved.find(r => r.id === id), edit)); pending.clear();
        return { saved: ids, missing: [], failed: [] };
      }
      if (command === 'list_luts') return structuredClone(saved);
      if (command === 'card_write_plan') return { items: [], blocked: [], files: [], preload_name: 'A', confirmation_token: 'once', savedStrength: saved[0].strength };
      if (command === 'card_apply') return { written: [], ph_menu_sha: 'abc', backup_dir: '/local-backup' };
      if (command === 'list_cards') return structuredClone(ctx.state.cards);
      if (command === 'card_list_get') return { mount: args.mount, items: [] };
      if (command === 'discard_edits') { pending.clear(); return 0; }
      throw new Error(command);
    }
  };
  vm.createContext(ctx);
  vm.runInContext(section('  function hash32(', '  function dispName(')
    + section('  var editQueue =', '  // ── 库渲染')
    + section('  function renderSelection()', '  async function applyOrder')
    + section('  // 输入当下绑定', "  $('fname').addEventListener")
    + section('  async function loadLuts()', '  // ── 回收站'), ctx);
  return { ctx, saved, pending, calls, el };
}
test('slider then immediate selection saves original ID', async () => {
  const { ctx, saved, calls } = setup();
  ctx.selected().strength = .8; ctx.scheduleEdit(); await ctx.select(2);
  assert.equal(saved[0].strength, .8); assert.equal(ctx.state.sel.id, 2);
  assert.equal(calls.filter(c => c.command === 'edit_lut').length, 1);
  assert.equal(calls.find(c => c.command === 'edit_lut').args.id, 1);
});
test('selection updates existing row highlights without rebuilding lists or moving focus', async () => {
  const { ctx } = setup();
  function row(dataset, active) {
    const badge = { hidden: !active };
    return { dataset, badge, classList: { contains: () => active, toggle: (_, value) => { active = value; } },
      querySelector: () => badge };
  }
  const rows = [row({ id: '1' }, true), row({ id: '2' }, false), row({ key: 'card:1' }, false)];
  ctx.document.querySelectorAll = () => rows;
  ctx.document.activeElement = rows[1];
  ctx.renderLib = ctx.renderTf = () => { throw Error('selection must preserve list DOM'); };
  await ctx.select(2);
  assert.deepEqual(rows.map(r => r.badge.hidden), [true, false, true]);
  assert.equal(ctx.document.activeElement, rows[1]);
  ctx.state.tf = { items: [{ key: 'card:1' }] };
  await ctx.selectCard('card:1');
  assert.deepEqual(rows.map(r => r.badge.hidden), [true, true, false]);
  await ctx.selectCard('card:1');
  assert.deepEqual(rows.map(r => r.badge.hidden), [true, true, false]);
});
test('immediate close flush waits for current edit to reach disk', async () => {
  const { ctx, saved } = setup();
  ctx.selected().strength = .9; ctx.scheduleEdit();
  assert.equal(await ctx.flushNow(), true); assert.equal(saved[0].strength, .9); assert.equal(ctx.state.dirty.size, 0);
});
test('new input during previous flush is saved before flushNow succeeds', async () => {
  const { ctx, saved, pending } = setup(), entered = deferred(), gate = deferred(); let first = true;
  ctx.hook = async cmd => { if (cmd === 'flush_edits' && first) { first = false; entered.resolve(); await gate.promise; } };
  ctx.selected().strength = .8; ctx.commitEdit(); const saving = ctx.flushNow(); await entered.promise;
  ctx.selected().strength = .95; const editing = ctx.commitEdit(); gate.resolve();
  assert.equal(await saving, true); await editing;
  assert.equal(saved[0].strength, .95); assert.equal(ctx.state.dirty.size, 0); assert.equal(pending.size, 0);
});
test('failed edit stays dirty and retry saves it; failed save blocks selection', async () => {
  const { ctx, saved } = setup(); let fail = true;
  ctx.hook = async cmd => { if (fail && cmd === 'edit_lut') throw Error('disk unavailable'); };
  ctx.selected().strength = .75; await ctx.commitEdit(); await ctx.select(2);
  assert.equal(ctx.state.sel.id, 1); assert.equal(ctx.state.dirty.size, 1);
  fail = false; assert.equal(await ctx.flushNow(), true); assert.equal(saved[0].strength, .75);
});
test('discard clears failed edits while keeping input entered during discard', async () => {
  const { ctx, saved, pending } = setup();
  vm.runInContext(section('  async function discardAll()', "  $('btn-discard').addEventListener"), ctx);
  ctx.hook = async cmd => { if (cmd === 'edit_lut') throw Error('disk unavailable'); };
  ctx.selected().strength = .75; await ctx.commitEdit();
  assert.equal(await ctx.discardAll(), true);
  assert.equal(ctx.state.dirty.size, 0); assert.equal(ctx.state.luts[0].strength, .65);
  ctx.hook = undefined;
  ctx.selected().strength = .8; await ctx.commitEdit();
  const entered = deferred(), gate = deferred();
  ctx.hook = async cmd => { if (cmd === 'discard_edits') { entered.resolve(); await gate.promise; } };
  const discarding = ctx.discardAll(); await entered.promise;
  ctx.selected().strength = .9; const editing = ctx.commitEdit(); gate.resolve();
  assert.equal(await discarding, true); await editing;
  assert.equal(ctx.state.dirty.size, 1); assert.equal(pending.get(1).strength, .9);
  assert.equal(await ctx.flushNow(), true); assert.equal(saved[0].strength, .9);
});
test('input during failed-edit retry is not marked saved by the older retry', async () => {
  const { ctx, saved, pending } = setup();
  ctx.hook = async cmd => { if (cmd === 'edit_lut') throw Error('unavailable'); };
  ctx.selected().strength = .75; await ctx.commitEdit();
  const entered = deferred(), gate = deferred(); let first = true;
  ctx.hook = async cmd => { if (cmd === 'edit_lut' && first) { first = false; entered.resolve(); await gate.promise; } };
  const saving = ctx.flushNow(); await entered.promise;
  ctx.selected().strength = .95; const editing = ctx.commitEdit(); gate.resolve();
  assert.equal(await saving, true); await editing;
  assert.equal(saved[0].strength, .95); assert.equal(pending.size, 0); assert.equal(ctx.state.dirty.size, 0);
});
test('input queued after discard request remains dirty even before discard starts', async () => {
  const { ctx, saved, pending } = setup();
  vm.runInContext(section('  async function discardAll()', "  $('btn-discard').addEventListener"), ctx);
  const entered = deferred(), gate = deferred(); let first = true;
  ctx.hook = async cmd => { if (cmd === 'edit_lut' && first) { first = false; entered.resolve(); await gate.promise; } };
  ctx.selected().strength = .75; const old = ctx.commitEdit(); await entered.promise;
  const discarding = ctx.discardAll();
  ctx.selected().strength = .95; const editing = ctx.commitEdit(); gate.resolve();
  await old; assert.equal(await discarding, true); await editing;
  assert.equal(ctx.state.dirty.size, 1); assert.equal(pending.get(1).strength, .95);
  assert.equal(await ctx.flushNow(), true); assert.equal(saved[0].strength, .95);
});
function writeHandlers(ctx) {
  vm.runInContext(section('  var writeBusy = false;', '  // ── 应用信息'), ctx);
}
test('write flow saves latest params before preparing and uses one confirmation token', async () => {
  const { ctx, calls, el, saved } = setup(); writeHandlers(ctx);
  let shown; ctx.confirmWrite = async p => { shown = p; return true; };
  ctx.selected().strength = .85; ctx.scheduleEdit();
  await el('tf-apply').handlers.click();
  assert.equal(saved[0].strength, .85); assert.equal(shown.savedStrength, .85);
  const commands = calls.map(c => c.command);
  assert(commands.indexOf('flush_edits') < commands.indexOf('card_write_plan'));
  assert.equal(calls.find(c => c.command === 'card_apply').args.confirmationToken, 'once');
});
test('cancel and duplicate clicks never submit additional writes', async () => {
  const { ctx, calls, el } = setup(); writeHandlers(ctx); const shown = deferred();
  ctx.confirmWrite = async () => shown.promise;
  const writing = el('tf-apply').handlers.click();
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  await el('tf-apply').handlers.click(); shown.resolve(false); await writing;
  assert.equal(calls.filter(c => c.command === 'card_apply').length, 0);
  assert.equal(calls.filter(c => c.command === 'card_write_plan').length, 1);
});
test('touch write requires explicit checkbox in confirmation dialog', async () => {
  const { ctx, el } = setup(); writeHandlers(ctx);
  const answer = ctx.confirmWrite({ items: [], files: [], touch_preview: { file: 'dxot/a.bin' } }, '/card-A');
  assert.equal(el('write-accept').disabled, true);
  el('write-touch').checked = true; el('write-touch').handlers.change.call(el('write-touch'));
  assert.equal(el('write-accept').disabled, false);
  el('write-accept').handlers.click(); assert.equal(await answer, true);
});
test('late card response is ignored and removal clears both card status and list', async () => {
  const { ctx, el } = setup();
  vm.runInContext(section('  function renderCardStatus()', '  // ── F-13 TF 卡区域')
    + section('  var scanSeq =', "  $('tf-write').addEventListener"), ctx);
  const old = deferred(); ctx.hook = async (cmd, args) => { if (cmd === 'card_list_get' && args.mount === '/card-A') return old.promise; };
  const loading = ctx.loadTf(); ctx.state.cardMount = '/card-B';
  ctx.state.cards = [{ mount_point: '/card-B', volume_name: 'B', device: 'disk2' }]; await ctx.loadTf();
  old.resolve({ mount: '/card-A', items: [] }); await loading;
  assert.equal(ctx.state.tf.mount, '/card-B'); assert.equal(el('status-card').textContent, 'CARD · disk2');
  ctx.state.cards = []; await ctx.loadCards();
  assert.equal(ctx.state.tf, null); assert.equal(ctx.state.cardMount, null); assert.equal(el('status-card').textContent, 'NO CARD');
});

function patchHandlers(ctx) {
  writeHandlers(ctx);
  ctx.state.patchModel = 'lightning';
  ctx.state.patchInfo = { build: '72', filters: ['orig', 'insta'] };
  const plan = { mount: '/card-A', device: 'disk1', firmware: '3.2.0 (ecef5ef809)', files: [
    { file: 'ph_menu.bin', action: '替换' }, { file: 'touch_prefs.bin', action: '保留设置' }
  ], create_dirs: ['PH_DIAG'], filesystem: 'exfat', free_mb: 100, current_build: '71', write_count: 18,
  preserve_prefs: true, confirmation_token: 'patch-once' };
  ctx.hook = async command => {
    if (command === 'patch_plan') return plan;
    if (command === 'patch_install') return { build: '72', backup_dir: '/local-backup', written: ['ph_menu.bin'] };
  };
  vm.runInContext(section('  // ── 补丁机型选择与结果提示', '  async function loadInfo()'), ctx);
  return plan;
}
test('patch preflight and cancel never submit installation', async () => {
  const { ctx, calls, el } = setup(); patchHandlers(ctx);
  await el('patch-check').handlers.click();
  assert.equal(calls.filter(c => c.command === 'patch_plan').length, 1);
  assert.equal(calls.filter(c => c.command === 'patch_install').length, 0);
  ctx.confirmPatch = async () => false;
  await el('patch-install').handlers.click();
  assert.equal(calls.filter(c => c.command === 'patch_install').length, 0);
  assert.match(el('patch-status').textContent, /已取消/);
});
test('patch installation saves pending edits and binds token and firmware; duplicate clicks blocked', async () => {
  const { ctx, calls, el, saved } = setup(); patchHandlers(ctx);
  const shown = deferred(), entered = deferred();
  ctx.confirmPatch = async () => { entered.resolve(); return shown.promise; };
  ctx.selected().strength = .9; ctx.scheduleEdit();
  const installing = el('patch-install').handlers.click(); await entered.promise;
  await el('patch-install').handlers.click(); shown.resolve(true); await installing;
  assert.equal(saved[0].strength, .9);
  const command = calls.find(c => c.command === 'patch_install');
  assert.equal(command.args.confirmationToken, 'patch-once');
  assert.equal(command.args.firmware, 'lightning-ecef5ef809');
  assert.equal(calls.filter(c => c.command === 'patch_install').length, 1);
  assert.match(el('patch-status').textContent, /安装完成/);
  assert.equal(el('patch-success').open, true);
  assert.match(el('patch-success-summary').textContent, /回读校验一致/);
  assert.match(el('patch-success-backup').textContent, /local-backup/);
});
test('patch dialog requires fresh compatibility acknowledgement each time', async () => {
  const { ctx, el } = setup(); const p = patchHandlers(ctx);
  let answer = ctx.confirmPatch(p, '/card-A');
  assert.equal(el('patch-accept').disabled, true);
  el('patch-accept').handlers.click(); assert.notEqual(el('patch-confirm').returnValue, 'install');
  el('patch-compatible').checked = true; el('patch-compatible').handlers.change.call(el('patch-compatible'));
  el('patch-accept').handlers.click(); assert.equal(await answer, true);
  answer = ctx.confirmPatch(p, '/card-A'); assert.equal(el('patch-compatible').checked, false);
  el('patch-cancel').handlers.click(); assert.equal(await answer, false);
});
test('patch card change during confirmation and absent card never install', async () => {
  const { ctx, calls, el } = setup(); patchHandlers(ctx);
  ctx.confirmPatch = async () => { ctx.state.cardMount = '/card-B'; return true; };
  await el('patch-install').handlers.click();
  assert.equal(calls.filter(c => c.command === 'patch_install').length, 0);
  assert.match(el('patch-status').textContent, /存储卡已切换/);
  ctx.state.cardMount = null; const count = calls.length;
  await el('patch-install').handlers.click(); assert.equal(calls.length, count);
});
test('model selection blocks patch operations until Lightning is selected; USB-C stays in picker', async () => {
  const { ctx, calls, el } = setup(); patchHandlers(ctx);
  vm.runInContext(section('  function renderCardStatus()', '  // ── F-13 TF 卡区域'), ctx);
  ctx.loadCards = async () => { ctx.cardLoads = (ctx.cardLoads || 0) + 1; };
  ctx.setPatchModel(null);
  assert.equal(el('patch-model-picker').hidden, false);
  assert.equal(el('patch-install').disabled, true);
  await ctx.runPatch(true); assert.equal(calls.length, 0);
  el('patch-model-usbc').handlers.click();
  assert.equal(el('panel-patch').dataset.model, 'usbc');
  assert.equal(el('patch-model-picker').hidden, false);
  await ctx.runPatch(false); await ctx.runPatch(true);
  assert.equal(calls.length, 0); assert.equal(ctx.cardLoads, undefined);
  el('patch-model-lightning').handlers.click();
  assert.equal(el('patch-model-picker').hidden, true); assert.equal(ctx.cardLoads, 1);
  assert.equal(el('patch-install').disabled, false);
  el('patch-model-change').handlers.click();
  assert.equal(ctx.state.patchModel, null); assert.equal(el('patch-model-picker').hidden, false);
  assert.equal(el('patch-install').disabled, true);
});
test('completion dialog preserves warnings and stays visible until acknowledged', async () => {
  const { ctx, el } = setup(); patchHandlers(ctx);
  ctx.showPatchSuccess({ build: '72', written: ['ph_menu.bin'], backup_dir: '/backup', warning: '请安全弹出后再拔卡' }, '/card-A');
  assert.equal(el('patch-success').open, true);
  assert.equal(el('patch-success-warning').hidden, false);
  assert.match(el('patch-success-warning').textContent, /安全弹出/);
  el('patch-success-close').handlers.click(); assert.equal(el('patch-success').open, false);
});
test('camera name saves uppercase to card plan without writing card; invalid or duplicate names preserve saved value', async () => {
  const { ctx, calls } = setup();
  ctx.MENU_CHARSET = '-025ABEFGHIKLNOPRSTUVWY';
  ctx.state.tf = { items: [{ key: 'local:1', menu: 'FLAT' }] };
  ctx.afterTfChange = () => {};
  vm.runInContext(section('  var cardQueue =', '  function afterTfChange('), ctx);
  ctx.hook = async (cmd, args) => {
    if (cmd === 'card_list_set_menu_name') {
      if (args.name === 'B-W') throw Error('名称重复');
      return { items: [{ key: args.key, menu: args.name }] };
    }
  };
  const input = { value: ' fuyuno ' };
  await ctx.tfSetMenuName('local:1', input);
  assert.equal(ctx.state.tf.items[0].menu, 'FUYUNO');
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [{ command: 'card_list_set_menu_name', args: { mount: '/card-A', key: 'local:1', name: 'FUYUNO' } }]);
  for (const invalid of ['GOLD', '中文', 'FLAT 2', 'FUYUNOO']) {
    input.value = invalid; await ctx.tfSetMenuName('local:1', input);
    assert.equal(input.value, 'FUYUNO');
  }
  assert.equal(calls.length, 1);
  input.value = 'B-W'; await ctx.tfSetMenuName('local:1', input);
  assert.equal(input.value, 'FUYUNO'); assert.equal(ctx.state.tf.items[0].menu, 'FUYUNO');
});
