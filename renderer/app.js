const SOURCE_LABEL = { user: '自己装的', plugin: '插件带的', builtin: '工具自带', project: '项目级' };

const state = { view: { rows: [], categories: [], hasKey: false }, cat: '全部', order: 'desc', q: '', status: { phase: 'idle' } };
const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function matchesQuery(r) {
  if (!state.q) return true;
  const q = state.q.toLowerCase();
  return r.name.toLowerCase().includes(q) || r.zh.toLowerCase().includes(q) || r.description.toLowerCase().includes(q);
}

function sortRows(rows) {
  const dir = state.order === 'desc' ? -1 : 1;
  return rows.sort((a, b) => (a.count - b.count) * dir || a.name.localeCompare(b.name));
}

function renderCats(searched) {
  const counts = { 全部: searched.length };
  for (const r of searched) counts[r.category] = (counts[r.category] || 0) + 1;
  const cats = ['全部', ...state.view.categories];
  $('#cats').innerHTML = cats
    .filter(c => c === '全部' || c !== '未分类' || counts[c])
    .map(c => {
      const n = counts[c] || 0;
      const cls = ['cat', c === state.cat ? 'on' : '', n ? '' : 'zero'].join(' ');
      return `<button class="${cls}" data-cat="${esc(c)}"><span>${esc(c)}</span><span class="n">${n}</span></button>`;
    }).join('');
}

function renderRows(rows) {
  $('#empty').hidden = rows.length > 0;
  $('#rows').innerHTML = rows.map((r, i) => {
    const desc = r.zh
      ? `<div class="desc" title="${esc(r.description)}">${esc(r.zh)}</div>`
      : `<div class="desc en" title="${esc(r.description)}">${esc(r.description || '（没有描述）')}</div>`;
    const tools = r.tools.map(t => `<span class="tool">${esc(t)}</span>`).join('');
    const cat = r.category === '未分类' ? `<span class="tag none">未分类</span>` : `<span class="tag">${esc(r.category)}</span>`;
    const title = Object.entries(r.byTool || {}).map(([t, n]) => `${t}: ${n} 次`).join('\n');
    return `<tr data-id="${esc(r.id)}">
      <td class="rank">${i + 1}</td>
      <td><div class="name">${esc(r.name)}</div><div class="meta">${tools}<span class="src">${SOURCE_LABEL[r.source] || ''}</span></div></td>
      <td>${desc}</td>
      <td>${cat}${r.manual ? '<span class="edited" title="手动编辑过，自动翻译不会覆盖">已编辑</span>' : ''}</td>
      <td class="num ${r.count ? '' : 'zero'}" title="${esc(title)}">${r.count}</td>
    </tr>`;
  }).join('');
}

function renderFoot() {
  const rows = state.view.rows;
  const used = rows.filter(r => r.count > 0).length;
  const s = state.status;
  let line = `共 ${rows.length} 个 skill，用过 ${used} 个`;
  if (s.phase === 'scan') line = '正在扫描 skill…';
  else if (s.phase === 'usage' && s.total) line = `正在读取使用记录 ${s.done}/${s.total}`;
  else if (s.phase === 'translate') line = `正在翻译 ${s.done}/${s.total}`;
  $('#foot').textContent = line;
  $('#rescan').classList.toggle('spin', ['scan', 'usage', 'translate'].includes(s.phase));

  const notice = $('#notice');
  if (s.phase === 'error') {
    notice.hidden = false;
    notice.textContent = s.message;
  } else if (!state.view.hasKey && rows.length) {
    notice.hidden = false;
    notice.innerHTML = '还没有配置 DeepSeek API Key，描述暂时显示英文原文、分类显示“未分类”。<a id="notice-set">去设置</a>';
    $('#notice-set').onclick = openSettings;
  } else {
    notice.hidden = true;
  }
}

function render() {
  const searched = state.view.rows.filter(matchesQuery);
  if (state.cat !== '全部' && !searched.some(r => r.category === state.cat) && !state.view.categories.includes(state.cat)) state.cat = '全部';
  renderCats(searched);
  const shown = state.cat === '全部' ? searched : searched.filter(r => r.category === state.cat);
  renderRows(sortRows([...shown]));
  renderFoot();
}

async function openSettings() {
  const s = await window.api.getSettings();
  $('#s-key').value = '';
  $('#s-key').placeholder = s.hasKey ? '已保存（留空则不修改）' : 'sk-…';
  $('#s-base').value = s.baseUrl;
  $('#s-model').value = s.model;
  $('#s-cats').value = s.categories.join('\n');
  $('#settings').returnValue = '';
  $('#settings').showModal();
}

$('#settings').addEventListener('close', () => {
  if ($('#settings').returnValue !== 'save') return;
  const key = $('#s-key').value.trim();
  window.api.saveSettings({
    baseUrl: $('#s-base').value.trim(),
    model: $('#s-model').value.trim(),
    apiKey: key ? key : undefined,
    categories: $('#s-cats').value.split('\n').map(x => x.trim()).filter(Boolean),
  });
});

// 点击某一行：编辑中文描述和分类
let editing = null;
function openEdit(id) {
  const r = state.view.rows.find(x => x.id === id);
  if (!r) return;
  editing = r.id;
  $('#e-name').textContent = r.name;
  $('#e-en').textContent = r.description || '（没有英文描述）';
  $('#e-zh').value = r.zh;
  $('#e-cat').innerHTML = state.view.categories
    .map(c => `<option${c === r.category ? ' selected' : ''}>${esc(c)}</option>`).join('');
  $('#e-msg').textContent = '';
  $('#e-regen').disabled = !state.view.hasKey;
  $('#e-regen').title = state.view.hasKey ? '' : '先在设置里填 DeepSeek API Key';
  // 按 Esc 关闭时 returnValue 不会被重置，这里先清空，避免误保存
  $('#edit').returnValue = '';
  $('#edit').showModal();
}
$('#rows').addEventListener('click', e => {
  const tr = e.target.closest('tr[data-id]');
  if (tr) openEdit(tr.dataset.id);
});
$('#e-regen').addEventListener('click', async () => {
  const btn = $('#e-regen');
  btn.disabled = true;
  $('#e-msg').textContent = '正在生成…';
  try {
    const r = await window.api.regenerate(editing);
    $('#e-zh').value = r.zh;
    $('#e-cat').value = r.category;
    $('#e-msg').textContent = '已生成，确认后点保存。';
  } catch (err) {
    $('#e-msg').textContent = '生成失败：' + String(err.message || err).replace(/^Error invoking remote method '[^']+': (Error: )?/, '');
  }
  btn.disabled = false;
});
$('#edit').addEventListener('close', () => {
  if ($('#edit').returnValue !== 'save' || !editing) return;
  window.api.saveSkill({ id: editing, zh: $('#e-zh').value, category: $('#e-cat').value });
});

$('#cats').addEventListener('click', e => {
  const b = e.target.closest('.cat');
  if (!b) return;
  state.cat = b.dataset.cat;
  render();
});
$('#order').addEventListener('click', e => {
  const b = e.target.closest('button');
  if (!b) return;
  state.order = b.dataset.order;
  document.querySelectorAll('#order button').forEach(x => x.classList.toggle('on', x === b));
  render();
});
$('#q').addEventListener('input', e => { state.q = e.target.value.trim(); render(); });
$('#rescan').addEventListener('click', () => window.api.rescan());
$('#open-settings').addEventListener('click', openSettings);

window.api.onView(v => { state.view = v; render(); });
window.api.onStatus(s => { state.status = s; renderFoot(); });
