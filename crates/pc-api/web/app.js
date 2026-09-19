'use strict';

const $ = (s) => document.querySelector(s);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};

function bytes(n) {
  const K = 1024;
  if (n >= K ** 4) return (n / K ** 4).toFixed(1) + ' ТиБ';
  if (n >= K ** 3) return (n / K ** 3).toFixed(1) + ' ГиБ';
  if (n >= K ** 2) return (n / K ** 2).toFixed(1) + ' МиБ';
  if (n >= K) return (n / K).toFixed(1) + ' КиБ';
  return n + ' Б';
}

function plural(n, one, few, many) {
  const a = Math.abs(n);
  if (a % 100 >= 11 && a % 100 <= 14) return `${n} ${many}`;
  if (a % 10 === 1) return `${n} ${one}`;
  if (a % 10 >= 2 && a % 10 <= 4) return `${n} ${few}`;
  return `${n} ${many}`;
}

function when(ts) {
  if (!ts) return 'дата неизвестна';
  const d = new Date(ts * 1000);
  const p = (x) => String(x).padStart(2, '0');
  return `${p(d.getUTCDate())}.${p(d.getUTCMonth() + 1)}.${d.getUTCFullYear()} ` +
         `${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}

async function api(path, opts) {
  const r = await fetch(path, opts);
  const data = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok) throw new Error(data.error || r.statusText);
  return data;
}

// ---- overview -----------------------------------------------------------

async function renderOverview() {
  const root = $('#overview');
  root.replaceChildren(el('p', 'empty', 'Загрузка…'));
  let s;
  try {
    s = await api('/api/status');
  } catch (e) {
    root.replaceChildren(el('p', 'empty', 'Ошибка: ' + e.message));
    return;
  }

  const copies = s.roles.find((r) => r.removable);
  const cards = [
    ['Изображений в индексе', s.images.toLocaleString('ru'), ''],
    ['Семейств', s.families.toLocaleString('ru'), ''],
    ['С несколькими файлами', s.families_multi.toLocaleString('ru'), ''],
    ['Точные копии', copies ? bytes(copies.bytes) : '—', copies && copies.count ? 'good' : ''],
    ['Превью и кэши', bytes(s.derived_removable_bytes), s.derived_removable_bytes ? 'good' : ''],
    ['В карантине', bytes(s.quarantined_bytes), s.quarantined_bytes ? 'warn' : ''],
  ];

  const grid = el('div', 'cards');
  for (const [label, value, cls] of cards) {
    const c = el('div', 'card' + (cls ? ' ' + cls : ''));
    c.append(el('div', 'value', value), el('div', 'label', label));
    grid.append(c);
  }

  const frag = document.createDocumentFragment();
  frag.append(grid);

  if (s.roles.length) {
    frag.append(el('h2', null, 'Роли файлов'));
    const t = el('table');
    t.innerHTML = '<thead><tr><th>Роль</th><th class="num">Файлов</th>' +
      '<th class="num">Объём</th><th>По умолчанию</th></tr></thead>';
    const tb = el('tbody');
    for (const r of s.roles) {
      const tr = el('tr');
      tr.append(
        el('td', null, r.label),
        el('td', 'num', r.count.toLocaleString('ru')),
        el('td', 'num', bytes(r.bytes)),
        el('td', null, r.removable ? 'удаляется' : 'сохраняется'),
      );
      tb.append(tr);
    }
    t.append(tb);
    frag.append(t);
  }

  if (s.skipped || s.mislabelled) {
    const notes = el('p', 'empty');
    const parts = [];
    if (s.skipped) parts.push(`${plural(s.skipped, 'файл', 'файла', 'файлов')} пропущено (не изображения)`);
    if (s.mislabelled) parts.push(`у ${plural(s.mislabelled, 'файла', 'файлов', 'файлов')} расширение не совпало с содержимым`);
    notes.textContent = parts.join(' · ');
    frag.append(notes);
  }

  root.replaceChildren(frag);
}

// ---- families -----------------------------------------------------------

const state = { offset: 0, limit: 30, total: 0, all: false, cursor: 0, families: [] };

function memberNode(fam, m, isFirst) {
  const node = el('div', 'member' + (m.removable ? ' is-copy' : '') + (isFirst ? '' : ' derived'));

  const img = el('img');
  img.loading = 'lazy';
  img.alt = m.name;
  img.src = m.thumb ? `/api/thumb/${m.thumb}` :
    'data:image/svg+xml,%3Csvg xmlns="http://www.w3.org/2000/svg"/%3E';
  img.addEventListener('click', (e) => { e.stopPropagation(); openLightbox(m); });

  const role = el('div', 'role ' + m.role, m.role_label);

  const mid = el('div');
  mid.append(el('div', 'name', m.name), el('div', 'dir', m.dir));
  if (m.evidence && m.evidence.detail) {
    mid.append(el('div', 'why', 'связь: ' + m.evidence.detail));
  }

  const right = el('div', 'right');
  right.append(
    el('div', null, `${m.width}×${m.height}`),
    el('div', 'dir', bytes(m.size)),
  );
  if (m.is_keeper) right.append(el('div', 'keeper', '★ лучший'));

  node.append(img, role, mid, right);
  node.addEventListener('click', () => setKeeper(fam, m.file_id));
  return node;
}

function familyNode(fam, index) {
  const node = el('div', 'family' + (index === state.cursor ? ' cursor' : ''));
  node.dataset.index = index;

  const title = fam.members[0] ? fam.members[0].name : `Семейство ${fam.id}`;
  node.append(el('h3', null, title));

  const bits = [when(fam.taken_at), fam.camera || 'камера неизвестна',
    plural(fam.members.length, 'файл', 'файла', 'файлов'), bytes(fam.total_size)];
  if (fam.removable_bytes > 0) bits.push(`вернётся ${bytes(fam.removable_bytes)}`);
  node.append(el('div', 'meta', bits.join(' · ')));

  fam.members.forEach((m, i) => node.append(memberNode(fam, m, i === 0)));
  return node;
}

async function setKeeper(fam, fileId) {
  try {
    await api(`/api/families/${fam.id}/keeper`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ file_id: fileId }),
    });
    fam.members.forEach((m) => { m.is_keeper = m.file_id === fileId; });
    paintFamilies();
  } catch (e) {
    alert('Не удалось: ' + e.message);
  }
}

function paintFamilies() {
  const list = $('#familyList');
  if (!state.families.length) {
    list.replaceChildren(el('p', 'empty',
      'Семейства не построены. Выполните `photo-cleanup families build`.'));
    $('#page').textContent = '';
    return;
  }
  const frag = document.createDocumentFragment();
  state.families.forEach((f, i) => frag.append(familyNode(f, i)));
  list.replaceChildren(frag);

  const from = state.offset + 1;
  const to = Math.min(state.offset + state.families.length, state.total);
  $('#page').textContent = `${from}–${to} из ${state.total}`;
  $('#prev').disabled = state.offset === 0;
  $('#next').disabled = to >= state.total;
}

async function loadFamilies() {
  const q = new URLSearchParams({
    limit: state.limit, offset: state.offset, all: state.all,
  });
  const data = await api('/api/families?' + q);
  state.total = data.total;
  state.families = data.families;
  state.cursor = 0;
  paintFamilies();
}

function moveCursor(delta) {
  if (!state.families.length) return;
  state.cursor = Math.max(0, Math.min(state.families.length - 1, state.cursor + delta));
  paintFamilies();
  const node = document.querySelector(`.family[data-index="${state.cursor}"]`);
  if (node) node.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
}

/// Space cycles which member the family calls its best, which is the one
/// decision a review pass makes over and over.
function cycleKeeper() {
  const fam = state.families[state.cursor];
  if (!fam || fam.members.length < 2) return;
  const at = fam.members.findIndex((m) => m.is_keeper);
  const next = fam.members[(at + 1) % fam.members.length];
  setKeeper(fam, next.file_id);
}

// ---- derived ------------------------------------------------------------

async function renderDerived() {
  const root = $('#derived');
  root.replaceChildren(el('p', 'empty', 'Загрузка…'));
  let rows;
  try {
    rows = await api('/api/derived');
  } catch (e) {
    root.replaceChildren(el('p', 'empty', 'Ошибка: ' + e.message));
    return;
  }
  if (!rows.length) {
    root.replaceChildren(el('p', 'empty',
      'Ничего не найдено. Выполните `photo-cleanup scan --root ...`.'));
    return;
  }

  const groups = new Map();
  for (const b of rows) {
    if (!groups.has(b.kind)) groups.set(b.kind, []);
    groups.get(b.kind).push(b);
  }

  const frag = document.createDocumentFragment();
  for (const [, items] of groups) {
    const total = items.filter((b) => b.removable).reduce((a, b) => a + b.size, 0);
    const head = items[0].regenerable
      ? `${items[0].kind_label} — вернётся ${bytes(total)}`
      : `${items[0].kind_label} — НЕ УДАЛЯЕТСЯ`;
    frag.append(el('h2', null, head));

    const t = el('table');
    t.innerHTML = '<thead><tr><th>Путь</th><th class="num">Файлов</th>' +
      '<th class="num">Объём</th><th>Состояние</th></tr></thead>';
    const tb = el('tbody');
    for (const b of items.sort((a, b) => b.size - a.size)) {
      const tr = el('tr');
      const pathCell = el('td');
      pathCell.append(el('span', null, b.path.split('/').slice(-3).join('/')));
      if (b.blocked) pathCell.append(el('span', 'badge blocked', b.blocked));
      if (b.hint) pathCell.append(el('span', 'badge', b.hint));
      tr.append(
        pathCell,
        el('td', 'num', b.file_count.toLocaleString('ru')),
        el('td', 'num', bytes(b.size)),
        el('td', null, b.state === 'present'
          ? (b.removable ? 'можно перенести' : '—')
          : (b.state === 'quarantined' ? 'в карантине' : 'удалено')),
      );
      tb.append(tr);
    }
    t.append(tb);
    frag.append(t);
  }
  root.replaceChildren(frag);
}

// ---- lightbox -----------------------------------------------------------

function openLightbox(m) {
  $('#lightboxImg').src = `/api/file/${m.file_id}`;
  $('#lightboxCaption').textContent =
    `${m.name} · ${m.width}×${m.height} · ${bytes(m.size)} · ${m.breakdown}`;
  $('#lightbox').hidden = false;
}

// ---- wiring -------------------------------------------------------------

function showTab(name) {
  for (const b of document.querySelectorAll('#tabs button')) {
    b.classList.toggle('active', b.dataset.tab === name);
  }
  for (const s of document.querySelectorAll('.tab')) {
    s.classList.toggle('active', s.id === name);
  }
  if (name === 'overview') renderOverview();
  if (name === 'families') loadFamilies().catch((e) => {
    $('#familyList').replaceChildren(el('p', 'empty', 'Ошибка: ' + e.message));
  });
  if (name === 'derived') renderDerived();
}

document.addEventListener('DOMContentLoaded', () => {
  for (const b of document.querySelectorAll('#tabs button')) {
    b.addEventListener('click', () => showTab(b.dataset.tab));
  }
  $('#lightbox').addEventListener('click', () => { $('#lightbox').hidden = true; });
  $('#prev').addEventListener('click', () => {
    state.offset = Math.max(0, state.offset - state.limit);
    loadFamilies();
  });
  $('#next').addEventListener('click', () => {
    state.offset += state.limit;
    loadFamilies();
  });
  $('#showSingles').addEventListener('change', (e) => {
    state.all = e.target.checked;
    state.offset = 0;
    loadFamilies();
  });

  document.addEventListener('keydown', (e) => {
    if (!$('#lightbox').hidden) {
      if (e.key === 'Escape' || e.key === 'Enter') $('#lightbox').hidden = true;
      return;
    }
    if (!$('#families').classList.contains('active')) return;
    if (e.key === 'j' || e.key === 'ArrowDown') { moveCursor(1); e.preventDefault(); }
    if (e.key === 'k' || e.key === 'ArrowUp') { moveCursor(-1); e.preventDefault(); }
    if (e.key === ' ') { cycleKeeper(); e.preventDefault(); }
    if (e.key === 'Enter') {
      const fam = state.families[state.cursor];
      const best = fam && (fam.members.find((m) => m.is_keeper) || fam.members[0]);
      if (best) openLightbox(best);
      e.preventDefault();
    }
  });

  showTab('overview');
});
