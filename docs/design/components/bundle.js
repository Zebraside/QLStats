/* @ds-bundle: {"format":4,"namespace":"QLStats","components":[{"name":"LiveMatch"},{"name":"StatCard"},{"name":"LeaderList"},{"name":"PlayersTable"},{"name":"UpdateIndicator"},{"name":"EmptyState"}]} */
/*
 * QLStats — компоненты на чистом JS, без библиотек и сборки.
 * Каждый компонент — функция, которая возвращает готовый DOM-элемент:
 *   QLStats.LiveMatch(live, opts) · QLStats.StatCard(opts) · QLStats.LeaderList(players, opts)
 *   QLStats.PlayersTable(players, opts) · QLStats.UpdateIndicator(opts) · QLStats.EmptyState(opts)
 * Тикающие подписи (время матча, «обновлено N с назад», обратный отсчёт повтора)
 * обновляет QLStats.startTicker(root) раз в секунду, без перерисовки компонентов.
 * QLStats.sample — данные только для прототипа, в app.js не переносить.
 */
(function () {
  'use strict';
  var NS = (window.QLStats = window.QLStats || {});
  var uid = 0;
  function nextId(p) { uid += 1; return (p || 'qs') + '-' + uid; }

  /* ---------- DOM ---------- */
  function h(tag, props) {
    var el = document.createElement(tag);
    if (props) {
      for (var k in props) {
        if (!Object.prototype.hasOwnProperty.call(props, k)) continue;
        var v = props[k];
        if (v == null || v === false) continue;
        if (k === 'class') el.className = v;
        else if (k === 'text') el.textContent = v;
        else if (k === 'svg') el.innerHTML = v; /* только константы ICONS */
        else if (k.slice(0, 2) === 'on' && typeof v === 'function') el.addEventListener(k.slice(2), v);
        else if (k === 'dataset') { for (var d in v) el.dataset[d] = v[d]; }
        else el.setAttribute(k, v === true ? '' : String(v));
      }
    }
    for (var i = 2; i < arguments.length; i++) add(el, arguments[i]);
    return el;
  }
  function add(el, c) {
    if (c == null || c === false) return;
    if (Array.isArray(c)) { for (var i = 0; i < c.length; i++) add(el, c[i]); return; }
    el.appendChild(typeof c === 'object' ? c : document.createTextNode(String(c)));
  }
  function sr(text) { return h('span', { class: 'qs-sr' }, text); }

  /* ---------- Иконки: inline SVG 16×16, обводка 1.5, currentColor ---------- */
  var ICONS = {
    sort: '<path d="M5 6.5 8 3.5l3 3M5 9.5l3 3 3-3"/>',
    sortDesc: '<path d="M8 3v10M4 9l4 4 4-4"/>',
    sortAsc: '<path d="M8 13V3M4 7l4-4 4 4"/>',
    offline: '<path d="M6.5 9.5 9.5 6.5"/><path d="M7 4.5 8.6 2.9a2.5 2.5 0 0 1 3.5 3.5L10.5 8"/><path d="M9 11.5l-1.6 1.6a2.5 2.5 0 0 1-3.5-3.5L5.5 8"/><path d="M2 2l12 12"/>',
    crosshair: '<circle cx="8" cy="8" r="5"/><path d="M8 1v3.5M8 11.5V15M1 8h3.5M11.5 8H15"/>',
    warn: '<path d="M8 2 14.5 13.5h-13z"/><path d="M8 6.5v3.2M8 11.6v.1"/>',
    calendar: '<rect x="2.5" y="3.5" width="11" height="10"/><path d="M2.5 6.5h11M5.5 2v3M10.5 2v3"/>',
    refresh: '<path d="M13 8a5 5 0 1 1-1.6-3.7"/><path d="M13 2.5v3h-3"/>',
    hourglass: '<path d="M4.5 2h7M4.5 14h7M5 2c0 3 6 3 6 6s-6 3-6 6M11 2c0 3-6 3-6 6s6 3 6 6"/>'
  };
  function icon(name, cls) {
    return h('span', {
      class: 'qs-icon' + (cls ? ' ' + cls : ''), 'aria-hidden': 'true',
      svg: '<svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="square" stroke-linejoin="miter">' + ICONS[name] + '</svg>'
    });
  }

  /* ---------- Форматирование (ru-RU) ---------- */
  var nf = new Intl.NumberFormat('ru-RU');
  var fmt = {
    num: function (n) { return n == null ? '—' : nf.format(n); },
    /* доля 0..1 → «31,8 %» (неразрывный пробел), null → «—» */
    accuracy: function (a) {
      if (a == null || isNaN(a)) return '—';
      return (Math.round(a * 1000) / 10).toFixed(1).replace('.', ',') + ' %';
    },
    plural: function (n, forms) {
      var a = Math.abs(n) % 100, b = a % 10;
      if (a > 10 && a < 20) return forms[2];
      if (b > 1 && b < 5) return forms[1];
      if (b === 1) return forms[0];
      return forms[2];
    },
    clock: function (sec) {
      sec = Math.max(0, Math.floor(sec));
      var hh = Math.floor(sec / 3600), mm = Math.floor((sec % 3600) / 60), ss = sec % 60;
      function p(x) { return (x < 10 ? '0' : '') + x; }
      return (hh ? hh + ':' + p(mm) : p(mm)) + ':' + p(ss);
    },
    ago: function (sec) {
      sec = Math.max(0, Math.round(sec));
      if (sec < 60) return 'обновлено ' + sec + ' с назад';
      var m = Math.floor(sec / 60);
      if (m < 60) return 'обновлено ' + m + ' мин назад';
      return 'обновлено ' + Math.floor(m / 60) + ' ч назад';
    },
    retry: function (sec) {
      return 'не удалось обновить, повтор через ' + Math.max(0, Math.ceil(sec)) + ' с';
    },
    /* время по Москве: «20:52» */
    mskTime: function (iso) {
      try { return new Date(iso).toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', timeZone: 'Europe/Moscow' }); }
      catch (e) { return ''; }
    },
    limits: function (l) {
      if (!l) return '';
      var parts = [];
      if (l.frag) parts.push('фраги ' + l.frag);
      if (l.capture) parts.push('захваты ' + l.capture);
      if (l.round) parts.push('раунды ' + l.round);
      if (l.score) parts.push('очки ' + l.score);
      if (l.time_min) parts.push(l.time_min + ' мин');
      return parts.join(' · ');
    }
  };

  function Nick(name, cls) {
    return h('span', { class: 'qs-nick' + (cls ? ' ' + cls : ''), title: name }, name);
  }
  /* Крупное число: разряды разделены узким зазором, а не моноширинным пробелом */
  function bigNum(v) {
    var parts = fmt.num(v).split(/[\s\u00a0\u202f]/);
    var el = h('span', { class: 'qs-num' });
    parts.forEach(function (p, i) { if (i) el.appendChild(h('span', { class: 'qs-gap', 'aria-hidden': 'true' })); el.appendChild(document.createTextNode(p)); });
    return el;
  }
  function skel(w, cls) {
    return h('span', { class: 'qs-skel' + (cls ? ' ' + cls : ''), style: 'width:' + w, 'aria-hidden': 'true' });
  }

  /* ---------- Тикер ---------- */
  function tick(root, now) {
    root = root || document; now = now || Date.now();
    var els, i, el;
    els = root.querySelectorAll('[data-qs-ago]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = fmt.ago((now - Number(el.dataset.qsAgo)) / 1000); }
    els = root.querySelectorAll('[data-qs-retry]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = fmt.retry((Number(el.dataset.qsRetry) - now) / 1000); }
    els = root.querySelectorAll('[data-qs-countdown]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = Math.max(0, Math.ceil((Number(el.dataset.qsCountdown) - now) / 1000)) + ' с'; }
    els = root.querySelectorAll('[data-qs-elapsed]');
    for (i = 0; i < els.length; i++) {
      el = els[i];
      el.textContent = fmt.clock(Number(el.dataset.qsElapsed) + (now - Number(el.dataset.qsAt)) / 1000);
    }
  }
  function startTicker(root) {
    tick(root);
    var t = setInterval(function () { tick(root); }, 1000);
    return function stop() { clearInterval(t); };
  }

  /* =====================================================================
   * UpdateIndicator
   * opts: { status: 'ok'|'loading'|'error', updatedAt: ms, retryAt: ms }
   * ===================================================================== */
  function UpdateIndicator(o) {
    o = o || {};
    var st = o.status || 'ok';
    var el = h('p', { class: 'qs-upd', 'data-state': st });
    if (st === 'loading') {
      add(el, [h('span', { class: 'qs-upd-dot', 'aria-hidden': 'true' }), 'загрузка…']);
    } else if (st === 'error') {
      var retryAt = o.retryAt || Date.now() + 30000;
      add(el, [
        icon('warn'),
        h('span', { 'aria-hidden': 'true', dataset: { qsRetry: String(retryAt) } }, fmt.retry((retryAt - Date.now()) / 1000)),
        sr('Не удалось обновить статистику, повтор через 30 секунд. Показаны последние полученные данные.')
      ]);
      if (o.updatedAt) el.title = 'Показаны данные на ' + new Date(o.updatedAt).toLocaleTimeString('ru-RU');
    } else {
      var at = o.updatedAt || Date.now();
      add(el, [h('span', { class: 'qs-upd-dot', 'aria-hidden': 'true' }),
        h('span', { dataset: { qsAgo: String(at) } }, fmt.ago((Date.now() - at) / 1000))]);
    }
    return el;
  }

  /* =====================================================================
   * LeaderList
   * players: [{steam_id, name}], opts: { max: 3 }
   * ===================================================================== */
  function LeaderList(players, o) {
    o = o || {};
    var max = o.max || 3;
    players = players || [];
    var tie = players.length > 1;
    var ul = h('ul', { class: 'qs-leaders', 'aria-label': tie ? 'Лидеры, поровну: ' + players.length : 'Лидер' });
    var listId = nextId('leaders');
    ul.id = listId;
    var hiddenLis = [];
    players.forEach(function (p, i) {
      var li = h('li', { class: 'qs-leaders-item' }, Nick(p.name));
      if (players.length > max && i >= max) { li.hidden = true; hiddenLis.push(li); }
      ul.appendChild(li);
    });
    if (hiddenLis.length) {
      var rest = players.slice(max).map(function (p) { return p.name; });
      var n = hiddenLis.length;
      var btn = h('button', {
        type: 'button', class: 'qs-more', 'aria-expanded': 'false', 'aria-controls': listId,
        title: rest.join(', ')
      }, '+' + n, sr(' ' + fmt.plural(n, ['лидер', 'лидера', 'лидеров']) + ', показать'));
      btn.addEventListener('click', function () {
        var open = btn.getAttribute('aria-expanded') !== 'true';
        hiddenLis.forEach(function (li) { li.hidden = !open; });
        btn.setAttribute('aria-expanded', String(open));
        btn.firstChild.textContent = open ? 'свернуть' : '+' + n;
        btn.lastChild.textContent = open ? '' : ' ' + fmt.plural(n, ['лидер', 'лидера', 'лидеров']) + ', показать';
      });
      ul.appendChild(h('li', { class: 'qs-leaders-item qs-leaders-more' }, btn));
    }
    return ul;
  }

  /* =====================================================================
   * StatCard
   * opts: { label, value, unit: [одна, две, пять], leaders?: [] | undefined,
   *         emptyText?: 'нет матчей', loading?: bool }
   * leaders === undefined — карточка без списка («Матчей сегодня»).
   * ===================================================================== */
  function StatCard(o) {
    o = o || {};
    var card = h('article', { class: 'qs-card qs-plate' });
    if (o.loading) {
      card.setAttribute('aria-busy', 'true');
      add(card, [
        h('h3', { class: 'qs-card-label' }, o.label || skel('9rem')),
        h('p', { class: 'qs-card-value' }, skel('5.5rem', 'qs-skel-stat')),
        o.leaders === undefined ? null : h('p', { class: 'qs-card-sub' }, skel('7rem'))
      ]);
      return card;
    }
    var v = Number(o.value) || 0;
    var hasLeaders = o.leaders !== undefined;
    var empty = hasLeaders ? !(o.leaders && o.leaders.length) : v === 0;
    if (empty) card.classList.add('is-empty');
    add(card, [
      h('h3', { class: 'qs-card-label' }, o.label),
      h('p', { class: 'qs-card-value' },
        bigNum(v),
        o.unit ? h('span', { class: 'qs-card-unit' }, fmt.plural(v, o.unit)) : null)
    ]);
    if (hasLeaders) {
      if (o.leaders && o.leaders.length) add(card, LeaderList(o.leaders, { max: o.max || 3 }));
      else add(card, h('p', { class: 'qs-card-sub' }, o.emptyText || 'нет матчей'));
    }
    return card;
  }

  /* =====================================================================
   * EmptyState
   * opts: { kind: 'day' | 'error', retryAt?: ms, onRetry?: fn }
   * ===================================================================== */
  function EmptyState(o) {
    o = o || {};
    if (o.kind === 'error') {
      var retryAt = o.retryAt || Date.now() + 30000;
      return h('div', { class: 'qs-empty', 'data-kind': 'error', role: 'alert' },
        icon('warn', 'qs-empty-icon'),
        h('div', { class: 'qs-empty-text' },
          h('p', { class: 'qs-empty-title' }, 'Статистика пока недоступна'),
          h('p', { class: 'qs-empty-sub' }, 'Сервер статистики не ответил. Повторим автоматически через ',
            h('span', { class: 'qs-num', dataset: { qsCountdown: String(retryAt) } }, Math.max(0, Math.ceil((retryAt - Date.now()) / 1000)) + ' с'), '.')),
        h('button', { type: 'button', class: 'qs-btn', onclick: o.onRetry || null }, icon('refresh'), 'Повторить сейчас'));
    }
    return h('div', { class: 'qs-empty', 'data-kind': 'day' },
      icon('calendar', 'qs-empty-icon'),
      h('div', { class: 'qs-empty-text' },
        h('p', { class: 'qs-empty-title' }, 'Сегодня матчей ещё не было'),
        h('p', { class: 'qs-empty-sub' }, 'В статистику идут FFA и дуэли, где играли минимум двое. Матч появится здесь сразу после окончания.')));
  }

  /* =====================================================================
   * LiveMatch
   * live: ответ GET /api/live; opts: { loading?: bool, receivedAt?: ms }
   * ===================================================================== */
  var TEAM_NAME = { red: 'красные', blue: 'синие' };

  function liveScore(p, team) { var v = team ? (p.kills != null ? p.kills : p.score) : p.score; return v == null ? null : v; }

  function rosterRow(p, rank, opt) {
    var s = liveScore(p, opt.team);
    var lead = opt.leaderScore != null && s === opt.leaderScore;
    var li = h('li', { class: 'qs-ros-row' + (lead ? ' is-leader' : '') });
    if (opt.ranked) add(li, h('span', { class: 'qs-ros-rank qs-num', 'aria-hidden': 'true' }, rank));
    var name = h('span', { class: 'qs-ros-name' }, Nick(p.name));
    if (p.bot) add(name, h('span', { class: 'qs-tag' }, 'бот'));
    add(li, name);
    if (opt.team && (p.team === 'red' || p.team === 'blue')) {
      add(li, h('span', { class: 'qs-team', 'data-team': p.team, title: TEAM_NAME[p.team] },
        h('span', { class: 'qs-team-chip', 'aria-hidden': 'true' }), sr('команда: ' + TEAM_NAME[p.team])));
    }
    if (opt.scored) {
      add(li, h('span', { class: 'qs-ros-score qs-num' },
        sr(opt.team ? 'убийств: ' : 'текущий счёт: '), String(s == null ? '—' : s),
        lead ? sr(', лидер') : null));
    }
    return li;
  }

  function rosterList(players, opt, start, heading) {
    var col = h('div', { class: 'qs-ros-col' + (opt.ranked ? '' : ' is-unranked') });
    if (heading) add(col, heading);
    add(col, h('div', { class: 'qs-ros-head', 'aria-hidden': 'true' },
      opt.ranked ? h('span', { class: 'qs-ros-head-rank' }, '#') : null,
      h('span', { class: 'qs-ros-head-name' }, 'игрок'),
      opt.scored ? h('span', { class: 'qs-ros-head-score' }, opt.team ? 'убийства' : 'текущий') : null));
    var ol = h(opt.ranked ? 'ol' : 'ul', { class: 'qs-ros-list' });
    if (opt.ranked && start > 1) ol.setAttribute('start', start);
    players.forEach(function (p, i) { ol.appendChild(rosterRow(p, opt.ranks ? opt.ranks[start - 1 + i] : start + i, opt)); });
    add(col, ol);
    return col;
  }

  function competitionRanks(sorted, team) {
    var ranks = [], prev = null;
    sorted.forEach(function (p, i) { var s = liveScore(p, team); ranks.push(i > 0 && s === prev ? ranks[i - 1] : i + 1); prev = s; });
    return ranks;
  }

  function roster(players, mode) {
    var team = mode === 'team', scored = mode !== 'warmup';
    var list = players.slice();
    var leaderScore = null;
    if (scored) {
      list.sort(function (a, b) { return (liveScore(b, team) || 0) - (liveScore(a, team) || 0) || a.name.localeCompare(b.name, 'ru'); });
      var top = list.length ? liveScore(list[0], team) : null;
      leaderScore = top != null && top > 0 ? top : null;
    }
    var wrap = h('div', { class: 'qs-roster' });
    if (team) {
      wrap.dataset.cols = '2';
      ['red', 'blue'].forEach(function (t) {
        var members = list.filter(function (p) { return p.team === t; });
        var head = h('h3', { class: 'qs-ros-team', 'data-team': t },
          h('span', { class: 'qs-team-chip', 'aria-hidden': 'true' }), TEAM_NAME[t],
          h('span', { class: 'qs-ros-count qs-num' }, members.length));
        add(wrap, rosterList(members, { team: true, scored: true, ranked: false, leaderScore: leaderScore }, 1, head));
      });
      var free = list.filter(function (p) { return p.team !== 'red' && p.team !== 'blue'; });
      if (free.length) add(wrap, rosterList(free, { team: true, scored: true, ranked: false, leaderScore: leaderScore }, 1, h('h3', { class: 'qs-ros-team' }, 'без команды')));
      return wrap;
    }
    var opt = { team: false, scored: scored, ranked: scored, leaderScore: leaderScore, ranks: scored ? competitionRanks(list, false) : null };
    if (list.length > 6) {
      var half = Math.ceil(list.length / 2);
      wrap.dataset.cols = '2';
      add(wrap, [rosterList(list.slice(0, half), opt, 1), rosterList(list.slice(half), opt, half + 1)]);
    } else {
      add(wrap, rosterList(list, opt, 1));
    }
    return wrap;
  }

  function fact(label, value, cls) {
    return h('div', { class: 'qs-fact' + (cls ? ' ' + cls : '') }, h('dt', null, label), h('dd', null, value));
  }

  function LiveMatch(live, o) {
    o = o || {}; live = live || {};
    var st = o.loading ? 'loading' : (live.state || 'offline');
    var hid = nextId('live-h');
    var sec = h('section', { class: 'qs-live qs-plate', 'data-state': st, 'aria-labelledby': hid });
    if (st === 'loading') sec.setAttribute('aria-busy', 'true');

    var badge = null;
    if (st === 'offline') badge = h('span', { class: 'qs-badge qs-badge-off' }, icon('offline'), 'нет связи');
    else if (st !== 'loading') badge = h('span', { class: 'qs-badge qs-badge-live' }, h('span', { class: 'qs-live-dot', 'aria-hidden': 'true' }), 'live', sr(' — обновляется раз в 5 секунд'));
    add(sec, h('header', { class: 'qs-live-head' },
      h('h2', { id: hid, class: 'qs-h2' }, 'Текущий матч'), badge));

    var players = (live.players || []).filter(function (p) { return p.team !== 'spectator'; });
    var foot = function (note) {
      return h('footer', { class: 'qs-live-foot' },
        h('span', null, 'зрители: ', h('span', { class: 'qs-num' }, live.spectators || 0)),
        note ? h('span', { class: 'qs-live-note' }, note) : null);
    };

    if (st === 'loading') {
      add(sec, h('div', { class: 'qs-facts' }, [1, 2, 3, 4].map(function () { return h('div', { class: 'qs-fact' }, skel('3rem'), skel('6rem', 'qs-skel-lg')); })));
      add(sec, h('div', { class: 'qs-roster' }, h('div', { class: 'qs-ros-col' }, [1, 2, 3].map(function () { return h('div', { class: 'qs-ros-row' }, skel('60%')); }))));
      add(sec, sr('Загружаем текущий матч'));
      return sec;
    }

    if (st === 'offline') {
      var since = live.offline_since ? fmt.mskTime(live.offline_since) : '';
      add(sec, h('div', { class: 'qs-live-state' },
        icon('offline', 'qs-live-state-icon'),
        h('div', null,
          h('p', { class: 'qs-live-msg' }, 'Нет связи с игровым сервером'),
          h('p', { class: 'qs-live-sub' }, (since ? 'С ' + since + ' по Москве. ' : '') + 'Пробуем переподключиться. Статистика ниже доступна.'))));
      return sec;
    }

    if (st === 'empty') {
      add(sec, h('div', { class: 'qs-live-state' },
        icon('crosshair', 'qs-live-state-icon'),
        h('div', null,
          h('p', { class: 'qs-live-msg' }, 'На сервере никого нет. Заходи!'),
          live.server_title ? h('p', { class: 'qs-live-sub' }, live.server_title) : null)));
      if (live.spectators) add(sec, foot());
      return sec;
    }

    if (st === 'warmup') {
      add(sec, h('dl', { class: 'qs-facts' },
        fact('статус', h('span', { class: 'qs-live-msg-inline' }, icon('hourglass'), 'Разминка — ждём начала матча'), 'qs-fact-wide'),
        fact('карта', h('span', { class: 'qs-muted' }, 'неизвестна')),
        fact('игроков', h('span', { class: 'qs-num' }, players.length))));
      add(sec, roster(players, 'warmup'));
      add(sec, foot('Счёт появится, когда начнётся матч.'));
      return sec;
    }

    /* in_progress */
    var m = live.match || {};
    var team = !!m.team_mode;
    var base = 0;
    if (m.started_at && live.generated_at) base = (Date.parse(live.generated_at) - Date.parse(m.started_at)) / 1000;
    var at = o.receivedAt || Date.now();
    var limits = fmt.limits(m.limits);
    add(sec, h('dl', { class: 'qs-facts' },
      fact('карта', h('span', { class: 'qs-map' }, m.map || '—')),
      fact('режим', h('span', null, h('span', { class: 'qs-mode' }, m.game_type || '—'), m.factory_title ? h('span', { class: 'qs-muted' }, ' · ' + m.factory_title) : null)),
      fact('идёт', h('span', { class: 'qs-clock qs-num', dataset: { qsElapsed: String(base), qsAt: String(at) } }, fmt.clock(base + (Date.now() - at) / 1000))),
      limits ? fact('лимиты', h('span', { class: 'qs-limits' }, limits)) : null));
    add(sec, roster(players, team ? 'team' : 'ffa'));
    add(sec, foot(team
      ? 'Убийства — текущие и приближённые. Итог появится в статистике; командные режимы в неё не входят.'
      : 'Счёт текущий и приближённый — итоговый появится в статистике после матча.'));
    return sec;
  }

  /* =====================================================================
   * PlayersTable
   * players: ответ GET /api/players → players
   * opts: { sort: {key, dir}, storageKey?: string, loading?: bool, rows?: number, label?: string }
   * ===================================================================== */
  var COLUMNS = [
    { key: 'name', label: 'Ник', text: true },
    { key: 'matches', label: 'Матчи' },
    { key: 'kills', label: 'Убийства' },
    { key: 'deaths', label: 'Смерти' },
    { key: 'best_streak', label: 'Лучший стрик' },
    { key: 'wins', label: 'Победы' },
    { key: 'accuracy', label: 'Средняя точность (%)', format: fmt.accuracy },
    { key: 'suicides', label: 'Самоубийства' },
    { key: 'env_deaths', label: 'Смерти от окружения' }
  ];
  var DIR_RU = { ascending: 'по возрастанию', descending: 'по убыванию' };

  function compare(key, dir, text) {
    var m = dir === 'asc' ? 1 : -1;
    return function (a, b) {
      var x = a[key], y = b[key];
      var xn = x == null, yn = y == null;
      if (xn || yn) { if (xn && yn) return a.name.localeCompare(b.name, 'ru'); return xn ? 1 : -1; } /* null всегда внизу */
      var r = text ? String(x).localeCompare(String(y), 'ru', { sensitivity: 'base' }) : x - y;
      return r * m || a.name.localeCompare(b.name, 'ru');
    };
  }

  function PlayersTable(players, o) {
    o = o || {};
    players = players || [];
    var sort = o.sort || { key: 'kills', dir: 'desc' };
    if (o.storageKey) {
      try { var saved = JSON.parse(localStorage.getItem(o.storageKey) || 'null'); if (saved && saved.key) sort = saved; } catch (e) { /* хранилище недоступно */ }
    }
    var capId = nextId('pt');
    var box = h('div', { class: 'qs-table-box' });
    var scroller = h('div', { class: 'qs-table-scroll', tabindex: '0', role: 'region', 'aria-labelledby': capId });
    var table = h('table', { class: 'qs-table' });
    add(table, h('caption', { id: capId, class: 'qs-sr' }, (o.label || 'Игроки') + '. Нажмите на заголовок колонки, чтобы отсортировать.'));
    var colgroup = h('colgroup', null, COLUMNS.map(function (c) { return h('col', { class: c.text ? 'qs-col-nick' : 'qs-col-num' }); }));
    add(table, colgroup);
    var headRow = h('tr');
    var ths = {};
    COLUMNS.forEach(function (c) {
      var th = h('th', { scope: 'col', class: c.text ? 'qs-th-nick' : 'qs-th-num' });
      if (o.loading) { add(th, h('span', { class: 'qs-th-label' }, c.label)); }
      else {
        var btn = h('button', { type: 'button', class: 'qs-sort' }, h('span', { class: 'qs-sort-icon' }), h('span', { class: 'qs-th-label' }, c.label));
        btn.addEventListener('click', function () {
          if (sort.key === c.key) sort = { key: c.key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
          else sort = { key: c.key, dir: c.text ? 'asc' : 'desc' };
          if (o.storageKey) { try { localStorage.setItem(o.storageKey, JSON.stringify(sort)); } catch (e) { /* ignore */ } }
          render();
          announce.textContent = 'Сортировка: ' + c.label + ', ' + DIR_RU[sort.dir === 'asc' ? 'ascending' : 'descending'] + '.';
        });
        add(th, btn);
      }
      ths[c.key] = th;
      headRow.appendChild(th);
    });
    add(table, h('thead', null, headRow));
    var tbody = h('tbody');
    add(table, tbody);
    var announce = h('p', { class: 'qs-sr', 'aria-live': 'polite' });

    function render() {
      COLUMNS.forEach(function (c) {
        var th = ths[c.key], active = sort.key === c.key;
        if (active) th.setAttribute('aria-sort', sort.dir === 'asc' ? 'ascending' : 'descending'); else th.removeAttribute('aria-sort');
        th.classList.toggle('is-sorted', active);
        var ic = th.querySelector('.qs-sort-icon');
        if (ic) { ic.innerHTML = ''; ic.appendChild(icon(active ? (sort.dir === 'asc' ? 'sortAsc' : 'sortDesc') : 'sort')); }
      });
      var rows = players.slice().sort(compare(sort.key, sort.dir, sort.key === 'name'));
      var frag = document.createDocumentFragment();
      rows.forEach(function (p) {
        var tr = h('tr');
        COLUMNS.forEach(function (c) {
          if (c.text) tr.appendChild(h('th', { scope: 'row', class: 'qs-td-nick' }, Nick(p.name)));
          else {
            var v = p[c.key];
            tr.appendChild(h('td', { class: 'qs-td-num' + (v == null ? ' is-null' : '') + (sort.key === c.key ? ' is-sorted' : '') },
              c.format ? c.format(v) : fmt.num(v),
              v == null ? sr('нет данных') : null));
          }
        });
        frag.appendChild(tr);
      });
      tbody.innerHTML = '';
      tbody.appendChild(frag);
    }

    if (o.loading) {
      table.setAttribute('aria-busy', 'true');
      var n = o.rows || 8;
      for (var i = 0; i < n; i++) {
        var tr = h('tr', { class: 'qs-skel-row' });
        COLUMNS.forEach(function (c, ci) {
          tr.appendChild(h(ci === 0 ? 'th' : 'td', { class: ci === 0 ? 'qs-td-nick' : 'qs-td-num' }, skel(ci === 0 ? (6 + ((i * 3) % 5)) + 'rem' : (2 + ((i + ci) % 3) * 0.6) + 'rem')));
        });
        tbody.appendChild(tr);
      }
    } else {
      render();
    }
    add(scroller, table);
    add(box, [scroller, announce]);
    return box;
  }

  /* ---------- Заголовок секции (общая разметка) ---------- */
  function SectionHead(title, sub, id) {
    return h('header', { class: 'qs-section-head' },
      h('h2', { class: 'qs-h2', id: id || null }, title),
      sub ? h('p', { class: 'qs-section-sub' }, sub) : null);
  }

  /* =====================================================================
   * Page — сборка страницы, вариант B «Пульт». Это шаблон для index.html/app.js,
   * а не отдельный компонент.
   * d: {
   *   live, liveLoading,            — ответ /api/live или первая загрузка
   *   summary, players,             — последние полученные /api/summary и /api/players (null — данных нет)
   *   status: { status: 'ok'|'loading'|'error', updatedAt, retryAt } — для UpdateIndicator
   *   storageKey, onRetry
   * }
   * ===================================================================== */
  var M_FORMS = ['матч', 'матча', 'матчей'], W_FORMS = ['победа', 'победы', 'побед'], K_FORMS = ['убийство', 'убийства', 'убийств'];

  function summaryCards(part, day, loading) {
    part = part || { matches: 0, top_wins: { value: 0, players: [] }, top_kills: { value: 0, players: [] } };
    return h('div', { class: 'qs-cards' },
      StatCard({ label: day ? 'Матчей сегодня' : 'Матчей всего', value: part.matches, unit: M_FORMS, loading: loading }),
      StatCard({ label: 'Больше всего побед', value: part.top_wins.value, unit: W_FORMS, leaders: part.top_wins.players, loading: loading }),
      StatCard({ label: 'Больше всего убийств', value: part.top_kills.value, unit: K_FORMS, leaders: part.top_kills.players, loading: loading }));
  }

  function Page(d) {
    d = d || {};
    var st = d.status || { status: 'ok', updatedAt: Date.now() };
    var loading = st.status === 'loading';
    var noData = !loading && !d.summary && !d.players;
    var live = d.live || null;

    var header = h('header', { class: 'qs-header' },
      h('div', { class: 'qs-header-in' },
        h('h1', { class: 'qs-wordmark' }, 'QLStats'),
        live && live.server_title ? h('span', { class: 'qs-server', title: live.server_title }, live.server_title) : (d.liveLoading ? skel('8rem') : null),
        UpdateIndicator(st)));

    var summaryCol = h('div', { class: 'qs-summary' });
    if (noData) {
      add(summaryCol, h('section', { 'aria-labelledby': 'qs-h-stats' },
        SectionHead('Статистика', null, 'qs-h-stats'),
        EmptyState({ kind: 'error', retryAt: st.retryAt, onRetry: d.onRetry })));
    } else {
      var s = d.summary || {};
      var dayEmpty = !loading && s.day && s.day.matches === 0;
      add(summaryCol, [
        h('section', { 'aria-labelledby': 'qs-h-day', 'aria-busy': loading ? 'true' : null },
          SectionHead('Статистика дня', 'день по московскому времени, сброс в 00:00', 'qs-h-day'),
          summaryCards(s.day, true, loading),
          dayEmpty ? EmptyState({ kind: 'day' }) : null),
        h('section', { 'aria-labelledby': 'qs-h-all', 'aria-busy': loading ? 'true' : null },
          SectionHead('За всё время', null, 'qs-h-all'),
          summaryCards(s.all_time, false, loading))
      ]);
    }

    var main = h('main', { class: 'qs-page' },
      h('div', { class: 'qs-top' }, LiveMatch(live, { loading: !!d.liveLoading, receivedAt: d.liveReceivedAt }), summaryCol));
    if (!noData) {
      add(main, h('section', { class: 'qs-players', 'aria-labelledby': 'qs-h-players' },
        SectionHead('Игроки', 'засчитываются FFA и дуэли · сортировка — клик по заголовку колонки', 'qs-h-players'),
        PlayersTable(d.players || [], { loading: loading, rows: 10, storageKey: d.storageKey, label: 'Игроки' })));
    }
    return h('div', { class: 'qs-app' }, header, main);
  }

  /* ---------- Данные для прототипа ---------- */
  function sampleData() {
    var seed = 20261018;
    function rnd() { seed = (seed * 1103515245 + 12345) % 2147483648; return seed / 2147483648; }
    function between(a, b) { return a + rnd() * (b - a); }
    var names = ['Anarki', 'vancho.dovlatov.ru', 'Drowsy', 'xX_Railgun_Enjoyer_From_Novosibirsk_Xx', 'Kotofey', 'bazooka_babushka', 'Sever', 'Lynx', 'pchelka', 'Mr.Gauss', 'Tolik', 'zzz', 'Shrapnel', 'Grin', 'Varvar', 'nik0', 'Morozko', 'Fenix', 'Ryba', 'Bublik', 'Sanya_74', 'Kirpich', 'Hook', 'Oleg', 'Strelok', 'tank', 'GoodGame', 'Murka', 'Pila', 'Gruz', 'Beton', 'Ice', 'Kaktus', 'Dimon', 'Avtomat', 'shaurma_enjoyer', 'Lom', 'Volk', 'ProkHor', 'Zlo', 'Ezh', 'Raketa', 'Nebo', 'Vint', 'Kuznec', 'BigMike', 'Leshiy', 'Termit', 'Sova', 'Oskolok', 'Mayak', 'Krot', 'Skif', 'Fokus', 'Uzel', 'Gvozd', 'Otvertka', 'Plazma'];
    var players = [
      { steam_id: '76561198000000002', name: 'Doom', matches: 230, kills: 5210, deaths: 4100, best_streak: 17, wins: 97, accuracy: 0.3184, suicides: 41, env_deaths: 12 },
      { steam_id: '76561198000000003', name: 'VeryLongNicknameFromTheServer_2026', matches: 3, kills: 12, deaths: 30, best_streak: 2, wins: 0, accuracy: null, suicides: 4, env_deaths: 1 }
    ];
    names.forEach(function (name, i) {
      var matches = Math.max(1, Math.round(Math.pow(rnd(), 1.8) * 210));
      var skill = between(0.35, 1.25);
      var kills = Math.min(5100, Math.round(matches * between(5, 19) * skill));
      var deaths = Math.round(kills * between(0.7, 1.5) / skill) + Math.round(matches * between(1, 4));
      var acc = matches < 3 && rnd() < 0.5 ? null : Math.round(between(0.17, 0.41) * 10000) / 10000;
      players.push({
        steam_id: '7656119800' + String(1000000 + i * 7919).slice(-7),
        name: name, matches: matches, kills: kills, deaths: deaths,
        best_streak: Math.max(1, Math.min(16, Math.round(between(1, 6) * skill * (1 + Math.log10(matches + 1) / 2)))),
        wins: Math.min(96, Math.round(matches * between(0, 0.42) * skill)),
        accuracy: acc,
        suicides: Math.round(matches * between(0, 0.22)),
        env_deaths: Math.round(matches * between(0, 0.07))
      });
    });
    players[2].name = 'Anarki';

    var lp = function (id, name, team, score, bot, kills) { return { steam_id: bot ? '0' : id, name: name, bot: !!bot, team: team, score: score, kills: kills == null ? score : kills }; };
    var live = {
      ffa: {
        generated_at: '2026-10-18T20:49:12+03:00', state: 'in_progress', server_title: 'Twitch server W',
        match: { map: 'campgrounds', game_type: 'FFA', factory_title: 'Free For All', team_mode: false, started_at: '2026-10-18T20:41:30+03:00', limits: { frag: 50, time_min: 15 } },
        players: [lp('76561198070782136', 'vancho.dovlatov.ru', 'free', 7), lp('76561198045366387', 'Drowsy', 'free', 5), lp('0', 'Ptica', 'free', 3, true)],
        spectators: 1, score_approximate: true
      },
      ffa12: {
        generated_at: '2026-10-18T21:02:40+03:00', state: 'in_progress', server_title: 'Twitch server W',
        match: { map: 'bloodrun', game_type: 'FFA', factory_title: 'Free For All', team_mode: false, started_at: '2026-10-18T20:51:05+03:00', limits: { frag: 50, time_min: 15 } },
        players: [
          lp('1', 'Doom', 'free', 31), lp('2', 'Anarki', 'free', 31), lp('3', 'vancho.dovlatov.ru', 'free', 24), lp('4', 'xX_Railgun_Enjoyer_From_Novosibirsk_Xx', 'free', 19),
          lp('5', 'Drowsy', 'free', 17), lp('0', 'Ptica', 'free', 15, true), lp('6', 'Kotofey', 'free', 12), lp('7', 'Sever', 'free', 9),
          lp('0', 'Sarge', 'free', 8, true), lp('8', 'pchelka', 'free', 4), lp('9', 'Mr.Gauss', 'free', 1), lp('10', 'Tolik', 'free', -2)
        ],
        spectators: 3, score_approximate: true
      },
      team12: {
        generated_at: '2026-10-18T21:20:15+03:00', state: 'in_progress', server_title: 'Twitch server W',
        match: { map: 'japanesecastles', game_type: 'CTF', factory_title: 'Capture The Flag', team_mode: true, started_at: '2026-10-18T21:08:44+03:00', limits: { capture: 8, time_min: 20 } },
        players: [
          lp('1', 'Doom', 'red', 14, false, 14), lp('2', 'bazooka_babushka', 'red', 9, false, 9), lp('3', 'Lynx', 'red', 8, false, 8), lp('4', 'VeryLongNicknameFromTheServer_2026', 'red', 6, false, 6),
          lp('0', 'Ptica', 'red', 5, true, 5), lp('5', 'Grin', 'red', 2, false, 2),
          lp('6', 'Anarki', 'blue', 12, false, 12), lp('7', 'Varvar', 'blue', 10, false, 10), lp('8', 'nik0', 'blue', 7, false, 7), lp('9', 'Morozko', 'blue', 7, false, 7),
          lp('0', 'Sarge', 'blue', 4, true, 4), lp('10', 'Fenix', 'blue', 1, false, 1)
        ],
        spectators: 2, score_approximate: true
      },
      warmup: {
        generated_at: '2026-10-18T20:39:50+03:00', state: 'warmup', server_title: 'Twitch server W', match: null,
        players: [lp('76561198045366387', 'Drowsy', 'free', null), lp('2', 'xX_Railgun_Enjoyer_From_Novosibirsk_Xx', 'free', null), lp('0', 'Ptica', 'free', null, true)],
        spectators: 0, score_approximate: true
      },
      empty: { generated_at: '2026-10-18T20:50:00+03:00', state: 'empty', server_title: 'Twitch server W', match: null, players: [], spectators: 0, score_approximate: true },
      offline: { generated_at: '2026-10-18T20:52:31+03:00', state: 'offline', server_title: 'Twitch server W', offline_since: '2026-10-18T20:52:03+03:00', match: null, players: [], spectators: 0, score_approximate: true }
    };
    var P = function (id, name) { return { steam_id: id, name: name }; };
    var summary = {
      normal: {
        generated_at: '2026-10-18T21:14:03+03:00',
        day: { date: '2026-10-18', matches: 7, top_wins: { value: 3, players: [P('76561198000000001', 'Anarki')] }, top_kills: { value: 58, players: [P('76561198000000001', 'Anarki'), P('76561198000000002', 'Doom')] } },
        all_time: { matches: 412, top_wins: { value: 97, players: [P('76561198000000002', 'Doom')] }, top_kills: { value: 5210, players: [P('76561198000000002', 'Doom')] } }
      },
      emptyDay: {
        generated_at: '2026-10-18T09:14:03+03:00',
        day: { date: '2026-10-18', matches: 0, top_wins: { value: 0, players: [] }, top_kills: { value: 0, players: [] } },
        all_time: { matches: 412, top_wins: { value: 97, players: [P('76561198000000002', 'Doom')] }, top_kills: { value: 5210, players: [P('76561198000000002', 'Doom')] } }
      },
      ties: {
        generated_at: '2026-10-18T23:40:00+03:00',
        day: { date: '2026-10-18', matches: 11, top_wins: { value: 2, players: [P('1', 'Doom'), P('2', 'xX_Railgun_Enjoyer_From_Novosibirsk_Xx'), P('3', 'Kotofey'), P('4', 'Sever'), P('5', 'pchelka')] }, top_kills: { value: 31, players: [P('6', 'VeryLongNicknameFromTheServer_2026')] } },
        all_time: { matches: 412, top_wins: { value: 97, players: [P('76561198000000002', 'Doom')] }, top_kills: { value: 5210, players: [P('76561198000000002', 'Doom')] } }
      }
    };
    return { players: players, live: live, summary: summary };
  }

  var cachedSample = null;
  Object.defineProperty(NS, 'sample', { configurable: true, enumerable: true, get: function () { return cachedSample || (cachedSample = sampleData()); } });

  NS.LiveMatch = LiveMatch;
  NS.StatCard = StatCard;
  NS.LeaderList = LeaderList;
  NS.PlayersTable = PlayersTable;
  NS.UpdateIndicator = UpdateIndicator;
  NS.EmptyState = EmptyState;
  NS.SectionHead = SectionHead;
  NS.Page = Page;
  NS.Nick = Nick;
  NS.icon = icon;
  NS.h = h;
  NS.fmt = fmt;
  NS.tick = tick;
  NS.startTicker = startTicker;
  NS.COLUMNS = COLUMNS;
})();
