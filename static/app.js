/*
 * QLStats — клиент страницы (вариант B «Пульт»). Vanilla JS, без библиотек и сборки.
 *
 * Данные:
 *   /api/live                    — раз в 5 с;
 *   /api/summary, /api/players   — при загрузке, раз в 30 с и при возвращении на вкладку.
 * Все запросы идут с If-None-Match; на 304 блоки не перерисовываются.
 * Время матча, «обновлено N с назад» и обратные отсчёты тикают локально раз в секунду.
 *
 * Безопасность: данные API (ники, названия) попадают в DOM только через textContent
 * и setAttribute. innerHTML используется лишь для констант-иконок ICONS.
 */
(function () {
  'use strict';

  var LIVE_EVERY = 5000;
  var STATS_EVERY = 30000;
  var REQUEST_TIMEOUT = 10000;
  /* Сколько можно показывать последний ответ /api/live, если новые не приходят */
  var LIVE_STALE_AFTER = 30000;
  var SORT_STORAGE_KEY = 'qlstats.players.sort';

  /* =====================================================================
   * DOM
   * ===================================================================== */
  function h(tag, props) {
    var el = document.createElement(tag);
    if (props) {
      for (var k in props) {
        if (!Object.prototype.hasOwnProperty.call(props, k)) continue;
        var v = props[k];
        if (v == null || v === false) continue;
        if (k === 'class') el.className = v;
        else if (k === 'dataset') { for (var d in v) el.dataset[d] = v[d]; }
        else if (k.slice(0, 2) === 'on' && typeof v === 'function') el.addEventListener(k.slice(2), v);
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
  function skel(w, cls) {
    return h('span', { class: 'qs-skel' + (cls ? ' ' + cls : ''), style: 'width:' + w, 'aria-hidden': 'true' });
  }
  function Nick(name) {
    name = name == null ? '' : String(name);
    return h('span', { class: 'qs-nick', title: name }, name);
  }

  /* ---------- Иконки: inline SVG 16×16, обводка 1.5, квадратные концы, currentColor ---------- */
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
    var el = h('span', { class: 'qs-icon' + (cls ? ' ' + cls : ''), 'aria-hidden': 'true' });
    el.innerHTML = '<svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" ' +
      'stroke-width="1.5" stroke-linecap="square" stroke-linejoin="miter">' + ICONS[name] + '</svg>';
    return el;
  }

  /* =====================================================================
   * Форматирование (ru-RU)
   * ===================================================================== */
  var NBSP = ' ';
  var nf = new Intl.NumberFormat('ru-RU');
  var fmt = {
    /* 5210 → «5 210», null → «—» */
    num: function (n) { return n == null || isNaN(n) ? '—' : nf.format(n); },
    /* доля 0..1 → «31,8 %», null → «—» */
    accuracy: function (a) {
      if (a == null || isNaN(a)) return '—';
      return (Math.round(a * 1000) / 10).toFixed(1).replace('.', ',') + NBSP + '%';
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
      if (sec < 60) return 'обновлено ' + sec + NBSP + 'с назад';
      var m = Math.floor(sec / 60);
      if (m < 60) return 'обновлено ' + m + NBSP + 'мин назад';
      return 'обновлено ' + Math.floor(m / 60) + NBSP + 'ч назад';
    },
    seconds: function (sec) { return Math.max(0, Math.ceil(sec)) + NBSP + 'с'; },
    retry: function (sec) { return 'не удалось обновить, повтор через ' + fmt.seconds(sec); },
    /* время по Москве: «20:52» */
    mskTime: function (iso) {
      var t = Date.parse(iso);
      if (isNaN(t)) return '';
      try { return new Date(t).toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', timeZone: 'Europe/Moscow' }); }
      catch (e) { return ''; }
    },
    /* нулевые и отсутствующие лимиты не показываются: «фраги 50 · 15 мин» */
    limits: function (l) {
      if (!l) return '';
      var parts = [];
      if (l.frag) parts.push('фраги ' + l.frag);
      if (l.capture) parts.push('захваты ' + l.capture);
      if (l.round) parts.push('раунды ' + l.round);
      if (l.score) parts.push('очки ' + l.score);
      if (l.time_min) parts.push(l.time_min + NBSP + 'мин');
      return parts.join(' · ');
    }
  };

  /* Крупное число: разряды разделены узким зазором .qs-gap, а не моноширинным пробелом */
  function bigNum(v) {
    var el = h('span', { class: 'qs-num' });
    fmt.num(v).split(/[\s  ]/).forEach(function (part, i) {
      if (i) el.appendChild(h('span', { class: 'qs-gap', 'aria-hidden': 'true' }));
      el.appendChild(document.createTextNode(part));
    });
    return el;
  }

  /* =====================================================================
   * Тикер: раз в секунду обновляет тикающие подписи, блоки не перерисовываются
   * ===================================================================== */
  /* Разница «часы сервера − локальные часы», мс. Нужна, чтобы время матча считалось
     от started_at сервера, даже если часы у зрителя спешат или отстают. */
  var serverOffset = 0, offsetKnown = false;

  function tick() {
    var now = Date.now(), els, i, el;
    els = document.querySelectorAll('[data-qs-ago]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = fmt.ago((now - Number(el.dataset.qsAgo)) / 1000); }
    els = document.querySelectorAll('[data-qs-retry]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = fmt.retry((Number(el.dataset.qsRetry) - now) / 1000); }
    els = document.querySelectorAll('[data-qs-countdown]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = fmt.seconds((Number(el.dataset.qsCountdown) - now) / 1000); }
    els = document.querySelectorAll('[data-qs-start]');
    for (i = 0; i < els.length; i++) { el = els[i]; el.textContent = fmt.clock((now + serverOffset - Number(el.dataset.qsStart)) / 1000); }
  }

  /* =====================================================================
   * Общие куски разметки
   * ===================================================================== */
  function SectionHead(title, sub, id) {
    return h('header', { class: 'qs-section-head' },
      h('h2', { class: 'qs-h2', id: id }, title),
      sub ? h('p', { class: 'qs-section-sub' }, sub) : null);
  }

  /* ---------- UpdateIndicator: состояние статистики в шапке ---------- */
  function renderIndicator(el, s) {
    el.replaceChildren();
    el.dataset.state = s.status;
    el.removeAttribute('title');
    if (s.status === 'loading') {
      add(el, [h('span', { class: 'qs-upd-dot', 'aria-hidden': 'true' }), 'загрузка…']);
    } else if (s.status === 'error') {
      add(el, [
        icon('warn'),
        h('span', { 'aria-hidden': 'true', dataset: { qsRetry: String(s.retryAt) } }, fmt.retry((s.retryAt - Date.now()) / 1000)),
        sr('Не удалось обновить статистику, повтор через 30 секунд. Показаны последние полученные данные.')
      ]);
      if (s.updatedAt) el.title = 'Показаны данные на ' + new Date(s.updatedAt).toLocaleTimeString('ru-RU');
    } else {
      add(el, [h('span', { class: 'qs-upd-dot', 'aria-hidden': 'true' }),
        h('span', { dataset: { qsAgo: String(s.updatedAt) } }, fmt.ago((Date.now() - s.updatedAt) / 1000))]);
    }
  }

  /* ---------- LeaderList: все лидеры, больше трёх — сворачиваются в «+N» ---------- */
  var leaderListId = 0;
  function LeaderList(players, max) {
    max = max || 3;
    var ul = h('ul', {
      class: 'qs-leaders', id: 'qs-leaders-' + (++leaderListId),
      'aria-label': players.length > 1 ? 'Лидеры, поровну: ' + players.length : 'Лидер'
    });
    var hidden = [];
    players.forEach(function (p, i) {
      var li = h('li', { class: 'qs-leaders-item' }, Nick(p.name));
      if (i >= max) { li.hidden = true; hidden.push(li); }
      ul.appendChild(li);
    });
    if (hidden.length) {
      var n = hidden.length;
      var moreText = ' ' + fmt.plural(n, ['лидер', 'лидера', 'лидеров']) + ', показать';
      var label = document.createTextNode('+' + n);
      var srText = sr(moreText);
      var btn = h('button', {
        type: 'button', class: 'qs-more', 'aria-expanded': 'false', 'aria-controls': ul.id,
        title: players.slice(max).map(function (p) { return p.name; }).join(', ')
      }, label, srText);
      btn.addEventListener('click', function () {
        var open = btn.getAttribute('aria-expanded') !== 'true';
        hidden.forEach(function (li) { li.hidden = !open; });
        btn.setAttribute('aria-expanded', String(open));
        label.textContent = open ? 'свернуть' : '+' + n;
        srText.textContent = open ? '' : moreText;
      });
      ul.appendChild(h('li', { class: 'qs-leaders-item' }, btn));
    }
    return ul;
  }

  /* ---------- StatCard ----------
     leaders === undefined — карточка без списка лидеров («Матчей сегодня»). */
  function StatCard(o) {
    var card = h('article', { class: 'qs-card qs-plate' });
    var hasLeaders = o.leaders !== undefined;
    if (o.loading) {
      card.setAttribute('aria-busy', 'true');
      add(card, [
        h('h3', { class: 'qs-card-label' }, o.label),
        h('p', { class: 'qs-card-value' }, skel('5.5rem', 'qs-skel-stat')),
        hasLeaders ? h('p', { class: 'qs-card-sub' }, skel('7rem')) : null
      ]);
      return card;
    }
    var v = Number(o.value) || 0;
    var leaders = o.leaders || [];
    if (hasLeaders ? !leaders.length : v === 0) card.classList.add('is-empty');
    add(card, [
      h('h3', { class: 'qs-card-label' }, o.label),
      h('p', { class: 'qs-card-value' }, bigNum(v), h('span', { class: 'qs-card-unit' }, fmt.plural(v, o.unit)))
    ]);
    if (hasLeaders) {
      add(card, leaders.length ? LeaderList(leaders, 3) : h('p', { class: 'qs-card-sub' }, 'нет матчей'));
    }
    return card;
  }

  /* ---------- EmptyState ---------- */
  function EmptyState(kind, retryAt, onRetry) {
    if (kind === 'error') {
      return h('div', { class: 'qs-empty', 'data-kind': 'error', role: 'alert' },
        icon('warn', 'qs-empty-icon'),
        h('div', { class: 'qs-empty-text' },
          h('p', { class: 'qs-empty-title' }, 'Статистика пока недоступна'),
          h('p', { class: 'qs-empty-sub' }, 'Сервер статистики не ответил. Повторим автоматически через ',
            h('span', { class: 'qs-num', dataset: { qsCountdown: String(retryAt) } }, fmt.seconds((retryAt - Date.now()) / 1000)), '.')),
        h('button', { type: 'button', class: 'qs-btn', onclick: onRetry }, icon('refresh'), 'Повторить сейчас'));
    }
    return h('div', { class: 'qs-empty', 'data-kind': 'day' },
      icon('calendar', 'qs-empty-icon'),
      h('div', { class: 'qs-empty-text' },
        h('p', { class: 'qs-empty-title' }, 'Сегодня матчей ещё не было'),
        h('p', { class: 'qs-empty-sub' }, 'В статистику идут FFA и дуэли, где играли минимум двое. Матч появится здесь сразу после окончания.')));
  }

  /* =====================================================================
   * LiveMatch — блок «Текущий матч»
   * state: in_progress | warmup | empty | offline (из API), плюс loading и unavailable (клиент)
   * ===================================================================== */
  var TEAM_NAME = { red: 'красные', blue: 'синие' };

  function liveScore(p, team) {
    var v = team ? (p.kills != null ? p.kills : p.score) : p.score;
    return v == null ? null : v;
  }

  function rosterRow(p, rank, opt) {
    var s = liveScore(p, opt.team);
    var lead = opt.leaderScore != null && s === opt.leaderScore;
    var li = h('li', { class: 'qs-ros-row' + (lead ? ' is-leader' : '') });
    if (opt.ranked) add(li, h('span', { class: 'qs-ros-rank qs-num', 'aria-hidden': 'true' }, rank));
    var name = h('span', { class: 'qs-ros-name' }, Nick(p.name));
    if (p.bot) add(name, h('span', { class: 'qs-tag' }, 'бот'));
    add(li, name);
    if (opt.team && TEAM_NAME[p.team]) {
      add(li, h('span', { class: 'qs-team', 'data-team': p.team, title: TEAM_NAME[p.team] },
        h('span', { class: 'qs-team-chip', 'aria-hidden': 'true' }), sr('команда: ' + TEAM_NAME[p.team])));
    }
    if (opt.scored) {
      add(li, h('span', { class: 'qs-ros-score qs-num' },
        sr(opt.team ? 'убийств: ' : 'текущий счёт: '), s == null ? '—' : String(s),
        lead ? sr(', лидер') : null));
    }
    return li;
  }

  function rosterColumn(players, opt, start, heading) {
    var col = h('div', { class: 'qs-ros-col' + (opt.ranked ? '' : ' is-unranked') }, heading);
    add(col, h('div', { class: 'qs-ros-head', 'aria-hidden': 'true' },
      opt.ranked ? h('span', { class: 'qs-ros-head-rank' }, '#') : null,
      h('span', { class: 'qs-ros-head-name' }, 'игрок'),
      opt.scored ? h('span', { class: 'qs-ros-head-score' }, opt.team ? 'убийства' : 'текущий') : null));
    var list = h(opt.ranked ? 'ol' : 'ul', { class: 'qs-ros-list' });
    if (opt.ranked && start > 1) list.setAttribute('start', start);
    players.forEach(function (p, i) {
      list.appendChild(rosterRow(p, opt.ranks ? opt.ranks[start - 1 + i] : start + i, opt));
    });
    add(col, list);
    return col;
  }

  /* Места с ничьими: 1, 1, 3 */
  function competitionRanks(sorted) {
    var ranks = [];
    sorted.forEach(function (p, i) {
      ranks.push(i > 0 && liveScore(p, false) === liveScore(sorted[i - 1], false) ? ranks[i - 1] : i + 1);
    });
    return ranks;
  }

  /* mode: 'ffa' | 'team' | 'warmup' */
  function roster(players, mode) {
    var team = mode === 'team', scored = mode !== 'warmup';
    var list = players.slice();
    var leaderScore = null;
    if (scored) {
      list.sort(function (a, b) {
        return (liveScore(b, team) || 0) - (liveScore(a, team) || 0) || String(a.name).localeCompare(String(b.name), 'ru');
      });
      var top = list.length ? liveScore(list[0], team) : null;
      leaderScore = top != null && top > 0 ? top : null;
    }
    var wrap = h('div', { class: 'qs-roster' });
    if (team) {
      /* колонка на команду; командный счёт не показывается — до конца матча его нет */
      wrap.dataset.cols = '2';
      var opt = { team: true, scored: true, ranked: false, leaderScore: leaderScore };
      ['red', 'blue'].forEach(function (t) {
        var members = list.filter(function (p) { return p.team === t; });
        add(wrap, rosterColumn(members, opt, 1, h('h3', { class: 'qs-ros-team', 'data-team': t },
          h('span', { class: 'qs-team-chip', 'aria-hidden': 'true' }), TEAM_NAME[t],
          h('span', { class: 'qs-ros-count qs-num' }, members.length))));
      });
      var free = list.filter(function (p) { return !TEAM_NAME[p.team]; });
      if (free.length) add(wrap, rosterColumn(free, opt, 1, h('h3', { class: 'qs-ros-team' }, 'без команды')));
      return wrap;
    }
    var o = { team: false, scored: scored, ranked: scored, leaderScore: leaderScore, ranks: scored ? competitionRanks(list) : null };
    if (list.length > 6) {
      /* больше 6 участников — две колонки (1–6, 7–12) */
      var half = Math.ceil(list.length / 2);
      wrap.dataset.cols = '2';
      add(wrap, [rosterColumn(list.slice(0, half), o, 1), rosterColumn(list.slice(half), o, half + 1)]);
    } else {
      add(wrap, rosterColumn(list, o, 1));
    }
    return wrap;
  }

  function fact(label, value, cls) {
    return h('div', { class: 'qs-fact' + (cls ? ' ' + cls : '') }, h('dt', null, label), h('dd', null, value));
  }

  function liveStateMessage(iconName, title, sub) {
    return h('div', { class: 'qs-live-state' },
      icon(iconName, 'qs-live-state-icon'),
      h('div', null,
        h('p', { class: 'qs-live-msg' }, title),
        sub ? h('p', { class: 'qs-live-sub' }, sub) : null));
  }

  function LiveMatch(live, st) {
    var sec = h('section', { class: 'qs-live qs-plate', 'data-state': st, 'aria-labelledby': 'qs-h-live' });
    var badge = null;
    if (st === 'offline') badge = h('span', { class: 'qs-badge qs-badge-off' }, icon('offline'), 'нет связи');
    else if (st !== 'loading' && st !== 'unavailable') {
      badge = h('span', { class: 'qs-badge qs-badge-live' },
        h('span', { class: 'qs-live-dot', 'aria-hidden': 'true' }), 'live', sr(' — обновляется раз в 5 секунд'));
    }
    add(sec, h('header', { class: 'qs-live-head' }, h('h2', { id: 'qs-h-live', class: 'qs-h2' }, 'Текущий матч'), badge));

    if (st === 'loading') {
      sec.setAttribute('aria-busy', 'true');
      add(sec, [
        h('div', { class: 'qs-facts' }, [1, 2, 3, 4].map(function () {
          return h('div', { class: 'qs-fact' }, skel('3rem'), skel('6rem', 'qs-skel-lg'));
        })),
        h('div', { class: 'qs-roster' }, h('div', { class: 'qs-ros-col' }, [1, 2, 3].map(function () {
          return h('div', { class: 'qs-ros-row' }, skel('60%'));
        }))),
        sr('Загружаем текущий матч')
      ]);
      return sec;
    }

    if (st === 'unavailable') {
      add(sec, liveStateMessage('warn', 'Текущий матч недоступен',
        'Сервер статистики не ответил. Повторяем каждые 5' + NBSP + 'с.'));
      return sec;
    }

    if (st === 'offline') {
      var since = live.offline_since ? fmt.mskTime(live.offline_since) : '';
      add(sec, liveStateMessage('offline', 'Нет связи с игровым сервером',
        (since ? 'С ' + since + ' по Москве. ' : '') + 'Пробуем переподключиться. Статистика ниже доступна.'));
      return sec;
    }

    /* зрители в ростер не попадают, только в счётчик */
    var players = (live.players || []).filter(function (p) { return p.team !== 'spectator'; });
    var foot = function (note) {
      return h('footer', { class: 'qs-live-foot' },
        h('span', null, 'зрители: ', h('span', { class: 'qs-num' }, live.spectators || 0)),
        note ? h('span', { class: 'qs-live-note' }, note) : null);
    };

    if (st === 'empty') {
      add(sec, liveStateMessage('crosshair', 'Сейчас на сервере тренируется Птица!', live.server_title || null));
      if (live.spectators) add(sec, foot());
      return sec;
    }

    if (st === 'warmup') {
      add(sec, [
        h('dl', { class: 'qs-facts' },
          fact('статус', h('span', { class: 'qs-live-msg-inline' }, icon('hourglass'), 'Разминка — ждём начала матча'), 'qs-fact-wide'),
          fact('карта', h('span', { class: 'qs-muted' }, 'неизвестна')),
          fact('игроков', h('span', { class: 'qs-num' }, players.length))),
        roster(players, 'warmup'),
        foot('Счёт появится, когда начнётся матч.')
      ]);
      return sec;
    }

    /* in_progress */
    var m = live.match || {};
    var team = !!m.team_mode;
    var started = Date.parse(m.started_at);
    var limits = fmt.limits(m.limits);
    var clock = isNaN(started)
      ? h('span', { class: 'qs-clock qs-num' }, '—')
      : h('span', { class: 'qs-clock qs-num', dataset: { qsStart: String(started) } },
          fmt.clock((Date.now() + serverOffset - started) / 1000));
    add(sec, [
      h('dl', { class: 'qs-facts' },
        fact('карта', h('span', { class: 'qs-map' }, m.map || '—')),
        fact('режим', h('span', null,
          h('span', { class: 'qs-mode' }, m.game_type || '—'),
          m.factory_title ? h('span', { class: 'qs-muted' }, ' · ' + m.factory_title) : null)),
        fact('идёт', clock),
        limits ? fact('лимиты', h('span', { class: 'qs-limits' }, limits)) : null),
      roster(players, team ? 'team' : 'ffa'),
      foot(team
        ? 'Убийства — текущие и приближённые. Итог появится в статистике; командные режимы в неё не входят.'
        : 'Счёт текущий и приближённый — итоговый появится в статистике после матча.')
    ]);
    return sec;
  }

  /* =====================================================================
   * PlayersTable — создаётся один раз; при новых данных меняется только tbody,
   * поэтому фокус на заголовке и выбранная сортировка не теряются.
   * ===================================================================== */
  var COLUMNS = [
    { key: 'name', label: 'Ник', text: true },
    { key: 'matches', label: 'Матчи' },
    { key: 'kills', label: 'Убийства' },
    { key: 'deaths', label: 'Смерти' },
    { key: 'best_streak', label: 'Лучший стрик' },
    { key: 'wins', label: 'Победы' },
    { key: 'fav_weapon', label: 'Любимое оружие', weapon: true },
    { key: 'accuracy', label: 'Средняя точность (%)', format: fmt.accuracy },
    { key: 'suicides', label: 'Самоубийства' },
    { key: 'env_deaths', label: 'Смерти от окружения' }
  ];
  var DEFAULT_SORT = { key: 'kills', dir: 'desc' };

  function byName(a, b) { return String(a.name).localeCompare(String(b.name), 'ru', { sensitivity: 'base' }); }

  /* Значение для сортировки: у любимого оружия — число убийств; нет данных → null */
  function sortVal(p, key) {
    if (key === 'fav_weapon') return p.fav_weapon && p.fav_weapon.kills != null ? p.fav_weapon.kills : null;
    return p[key];
  }

  function compare(key, dir) {
    var m = dir === 'asc' ? 1 : -1;
    return function (a, b) {
      var x = sortVal(a, key), y = sortVal(b, key);
      var xn = x == null, yn = y == null;
      if (xn || yn) return xn && yn ? byName(a, b) : (xn ? 1 : -1); /* null всегда внизу */
      var r = key === 'name' ? byName(a, b) : x - y;
      return r * m || byName(a, b);
    };
  }

  function loadSort() {
    try {
      var s = JSON.parse(localStorage.getItem(SORT_STORAGE_KEY) || 'null');
      var known = s && COLUMNS.some(function (c) { return c.key === s.key; });
      if (known && (s.dir === 'asc' || s.dir === 'desc')) return { key: s.key, dir: s.dir };
    } catch (e) { /* хранилище недоступно или испорчено */ }
    return DEFAULT_SORT;
  }
  function saveSort(sort) {
    try { localStorage.setItem(SORT_STORAGE_KEY, JSON.stringify(sort)); } catch (e) { /* хранилище недоступно */ }
  }

  /* ---------- Любимое оружие: иконка img/weapons/<key>.png + число убийств ---------- */
  var WEAPONS = {
    GAUNTLET: ['Гаунтлет', 'G'], MACHINEGUN: ['Пулемёт', 'MG'], SHOTGUN: ['Дробовик', 'SG'],
    GRENADE: ['Гранатомёт', 'GL'], ROCKET: ['Ракетница', 'RL'], LIGHTNING: ['Шафт', 'LG'],
    RAILGUN: ['Рейлган', 'RG'], PLASMA: ['Плазмаган', 'PG'], BFG: ['BFG', 'BFG'],
    HMG: ['Тяжёлый пулемёт', 'HMG'], CHAINGUN: ['Чейнган', 'CG'], NAILGUN: ['Нейлган', 'NG'],
    PROXMINE: ['Прокс-мины', 'PM']
  };

  function FavWeapon(fw) {
    var key = fw && typeof fw.weapon === 'string' ? fw.weapon.toUpperCase() : '';
    var kills = fw && fw.kills != null ? Number(fw.kills) : NaN;
    if (!key || isNaN(kills)) return null;
    var info = WEAPONS[key] || [key.charAt(0) + key.slice(1).toLowerCase(), key];
    var label = info[0] + ' — ' + fmt.num(kills) + NBSP + fmt.plural(kills, K_FORMS);
    var abbr = h('span', { class: 'qs-weapon-abbr', 'aria-hidden': 'true', hidden: true }, info[1]);
    var img = h('img', {
      class: 'qs-weapon-icon', src: '/img/weapons/' + key.toLowerCase() + '.png', alt: info[0],
      height: '20', loading: 'lazy', decoding: 'async'
    });
    img.addEventListener('error', function () { img.hidden = true; abbr.hidden = false; });
    return h('span', { class: 'qs-weapon', title: label, 'aria-label': label, role: 'img' },
      img, abbr, h('span', { class: 'qs-weapon-kills', 'aria-hidden': 'true' }, fmt.num(kills)));
  }

  function createPlayersTable() {
    var sort = loadSort();
    var players = null;
    var ths = {};
    var tbody = h('tbody');
    var announce = h('p', { class: 'qs-sr', 'aria-live': 'polite' });
    var headRow = h('tr');

    COLUMNS.forEach(function (c) {
      var btn = h('button', { type: 'button', class: 'qs-sort' },
        h('span', { class: 'qs-sort-icon' }), h('span', { class: 'qs-th-label' }, c.label));
      /* <button> сам срабатывает на Enter и Space */
      btn.addEventListener('click', function () {
        if (sort.key === c.key) sort = { key: c.key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
        else sort = { key: c.key, dir: c.text ? 'asc' : 'desc' };
        saveSort(sort);
        renderHead();
        renderRows();
        announce.textContent = 'Сортировка: ' + c.label + ', ' + (sort.dir === 'asc' ? 'по возрастанию' : 'по убыванию') + '.';
      });
      ths[c.key] = h('th', { scope: 'col', class: c.text ? 'qs-th-nick' : 'qs-th-num' }, btn);
      headRow.appendChild(ths[c.key]);
    });

    var table = h('table', { class: 'qs-table' },
      h('caption', { id: 'qs-players-caption', class: 'qs-sr' }, 'Игроки. Нажмите на заголовок колонки, чтобы отсортировать.'),
      h('colgroup', null, COLUMNS.map(function (c) { return h('col', { class: c.text ? 'qs-col-nick' : 'qs-col-num' }); })),
      h('thead', null, headRow),
      tbody);
    var box = h('div', { class: 'qs-table-box' },
      h('div', { class: 'qs-table-scroll', tabindex: '0', role: 'region', 'aria-labelledby': 'qs-players-caption' }, table),
      announce);

    function renderHead() {
      COLUMNS.forEach(function (c) {
        var th = ths[c.key], active = sort.key === c.key;
        if (active) th.setAttribute('aria-sort', sort.dir === 'asc' ? 'ascending' : 'descending');
        else th.removeAttribute('aria-sort');
        th.classList.toggle('is-sorted', active);
        th.querySelector('.qs-sort-icon').replaceChildren(icon(active ? (sort.dir === 'asc' ? 'sortAsc' : 'sortDesc') : 'sort'));
      });
    }

    function renderRows() {
      if (!players) return;
      var frag = document.createDocumentFragment();
      if (!players.length) {
        frag.appendChild(h('tr', null, h('td', { class: 'qs-td-empty', colspan: COLUMNS.length }, 'Засчитанных матчей ещё не было.')));
      }
      players.slice().sort(compare(sort.key, sort.dir)).forEach(function (p) {
        var tr = h('tr');
        COLUMNS.forEach(function (c) {
          if (c.text) { tr.appendChild(h('th', { scope: 'row', class: 'qs-td-nick' }, Nick(p.name))); return; }
          if (c.weapon) {
            var fw = FavWeapon(p.fav_weapon);
            tr.appendChild(h('td', { class: 'qs-td-weapon' + (fw ? '' : ' is-null') }, fw || '—', fw ? null : sr('нет данных')));
            return;
          }
          var v = p[c.key];
          tr.appendChild(h('td', { class: 'qs-td-num' + (v == null ? ' is-null' : '') },
            c.format ? c.format(v) : fmt.num(v), v == null ? sr('нет данных') : null));
        });
        frag.appendChild(tr);
      });
      tbody.replaceChildren(frag);
    }

    function setLoading(rows) {
      table.setAttribute('aria-busy', 'true');
      var frag = document.createDocumentFragment();
      for (var i = 0; i < rows; i++) {
        var tr = h('tr');
        COLUMNS.forEach(function (c, ci) {
          tr.appendChild(h(ci === 0 ? 'th' : 'td', { class: ci === 0 ? 'qs-td-nick' : 'qs-td-num' },
            skel(ci === 0 ? (6 + ((i * 3) % 5)) + 'rem' : (2 + ((i + ci) % 3) * 0.6) + 'rem')));
        });
        frag.appendChild(tr);
      }
      tbody.replaceChildren(frag);
    }

    function setPlayers(list) {
      var prev = players;
      players = list;
      try { renderRows(); } catch (e) { players = prev; throw e; }
      table.removeAttribute('aria-busy');
    }

    renderHead();
    return { el: box, setLoading: setLoading, setPlayers: setPlayers };
  }

  /* =====================================================================
   * Сводки: «Статистика дня» и «За всё время»
   * ===================================================================== */
  var M_FORMS = ['матч', 'матча', 'матчей'], W_FORMS = ['победа', 'победы', 'побед'], K_FORMS = ['убийство', 'убийства', 'убийств'];

  function summaryCards(part, day, loading) {
    part = part || {};
    var wins = part.top_wins || {}, kills = part.top_kills || {};
    return h('div', { class: 'qs-cards' },
      StatCard({ label: day ? 'Матчей сегодня' : 'Матчей всего', value: part.matches, unit: M_FORMS, loading: loading }),
      StatCard({ label: 'Больше всего побед', value: wins.value, unit: W_FORMS, leaders: wins.players || [], loading: loading }),
      StatCard({ label: 'Больше всего убийств', value: kills.value, unit: K_FORMS, leaders: kills.players || [], loading: loading }));
  }

  function summarySections(summary) {
    var loading = !summary;
    var s = summary || {};
    var dayEmpty = !loading && s.day && !s.day.matches;
    return [
      h('section', { 'aria-labelledby': 'qs-h-day', 'aria-busy': loading ? 'true' : null },
        SectionHead('Статистика дня', 'день по московскому времени, сброс в 00:00', 'qs-h-day'),
        summaryCards(s.day, true, loading),
        dayEmpty ? EmptyState('day') : null),
      h('section', { 'aria-labelledby': 'qs-h-all', 'aria-busy': loading ? 'true' : null },
        SectionHead('За всё время', null, 'qs-h-all'),
        summaryCards(s.all_time, false, loading))
    ];
  }

  /* =====================================================================
   * Сеть: fetch с If-None-Match, таймаутом и сохранением ETag
   * ===================================================================== */
  var etags = {};

  /* → { changed: false } на 304, { changed: true, data, etag } на 200; бросает при ошибке.
     ETag сохраняет вызывающий (commitEtag) — только после того, как ответ удалось показать. */
  function getJSON(url) {
    var headers = {};
    if (etags[url]) headers['If-None-Match'] = etags[url];
    var ctrl = typeof AbortController === 'function' ? new AbortController() : null;
    var timer = ctrl ? setTimeout(function () { ctrl.abort(); }, REQUEST_TIMEOUT) : null;
    var sentAt = Date.now();
    return fetch(url, { headers: headers, cache: 'no-store', credentials: 'same-origin', signal: ctrl ? ctrl.signal : undefined })
      .then(function (res) {
        var date = Date.parse(res.headers.get('Date') || '');
        var meta = { serverNow: isNaN(date) ? null : date, localNow: (sentAt + Date.now()) / 2 };
        if (res.status === 304) return { changed: false, meta: meta };
        if (!res.ok) throw new Error(url + ': HTTP ' + res.status);
        return res.json().then(function (data) {
          return { changed: true, data: data, etag: res.headers.get('ETag'), meta: meta };
        });
      })
      .finally(function () { if (timer) clearTimeout(timer); });
  }
  function commitEtag(url, r) { if (r.etag) etags[url] = r.etag; }

  /* =====================================================================
   * Состояние страницы и рендер по блокам
   * ===================================================================== */
  var el = {
    server: document.getElementById('qs-server'),
    upd: document.getElementById('qs-upd'),
    live: document.getElementById('qs-live'),
    summary: document.getElementById('qs-summary'),
    players: document.getElementById('qs-players')
  };

  var state = {
    live: null,           /* последний ответ /api/live */
    liveOkAt: 0,          /* когда /api/live последний раз ответил (200 или 304) */
    liveView: null,       /* что сейчас нарисовано в блоке LiveMatch */
    summary: null,        /* последние полученные /api/summary и /api/players — при ошибке не обнуляются */
    players: null,
    stats: { status: 'loading', updatedAt: 0, retryAt: 0 },
    summaryView: null     /* 'loading' | 'error' | 'data' */
  };

  var table = createPlayersTable();
  el.players.appendChild(table.el);

  /* Сначала строится узел, потом меняется состояние: если ответ API неожиданной формы
     и построение бросит исключение, на экране и в state останется прежнее. */
  function renderLive(view, live) {
    var node = LiveMatch(live, view);
    el.live.replaceChildren(node);
    state.live = live;
    state.liveView = view;
    el.server.replaceChildren();
    el.server.removeAttribute('title');
    if (view === 'loading') {
      el.server.appendChild(skel('8rem'));
    } else if (live && live.server_title) {
      el.server.textContent = live.server_title;
      el.server.title = live.server_title;
    }
  }

  /* changed — пришли новые данные /api/summary; иначе блок перерисовывается,
     только если сменился вид (загрузка → ошибка → данные) */
  function renderSummary(changed) {
    var s = state.stats;
    if (state.summary) {
      if (state.summaryView === 'data' && !changed) return;
      el.summary.replaceChildren.apply(el.summary, summarySections(state.summary));
      state.summaryView = 'data';
    } else if (s.status === 'error') {
      if (state.summaryView === 'error') {
        /* уже показано: только сдвинуть отсчёт, чтобы role="alert" не зачитывался заново */
        var cd = el.summary.querySelector('[data-qs-countdown]');
        if (cd) cd.dataset.qsCountdown = String(s.retryAt);
        return;
      }
      state.summaryView = 'error';
      el.summary.replaceChildren(h('section', { 'aria-labelledby': 'qs-h-stats' },
        SectionHead('Статистика', null, 'qs-h-stats'),
        EmptyState('error', s.retryAt, retryNow)));
    } else if (state.summaryView !== 'loading') {
      state.summaryView = 'loading';
      el.summary.replaceChildren.apply(el.summary, summarySections(null));
    }
  }

  /* Нет ни сводок, ни таблицы — секция «Игроки» скрыта, EmptyState стоит на месте сводок.
     Сводки есть, а таблицы нет — EmptyState в самой секции «Игроки». */
  var playersError = null;
  function renderPlayersBlock() {
    var s = state.stats;
    var missing = !state.players && s.status === 'error';
    el.players.hidden = missing && !state.summary;
    table.el.hidden = missing;
    if (missing && state.summary) {
      if (playersError) {
        playersError.querySelector('[data-qs-countdown]').dataset.qsCountdown = String(s.retryAt);
      } else {
        playersError = EmptyState('error', s.retryAt, retryNow);
        el.players.appendChild(playersError);
      }
    } else if (playersError) {
      playersError.remove();
      playersError = null;
    }
  }

  /* ---------- /api/live ---------- */
  var liveTimer = 0, liveBusy = false;

  function liveViewFor(live) {
    var st = live && live.state;
    return st === 'in_progress' || st === 'warmup' || st === 'empty' || st === 'offline' ? st : 'offline';
  }

  function updateServerOffset(r) {
    /* Часы сервера берём из заголовка Date (он есть и у 304). generated_at — время снапшота,
       оно не меняется между событиями, поэтому годится лишь как запасной вариант и только когда
       оно впереди локальных часов: такое расхождение — точно сдвиг часов, а не старый снапшот. */
    var next = null;
    if (r.meta.serverNow != null) {
      /* Date отброшены миллисекунды: +500 мс — середина секунды */
      next = r.meta.serverNow + 500 - r.meta.localNow;
    } else if (r.changed && r.data && r.data.generated_at) {
      var gen = Date.parse(r.data.generated_at);
      if (!isNaN(gen)) next = Math.max(0, gen - r.meta.localNow);
    }
    /* Смещение меняем только при заметной разнице, иначе таймер матча дрожал бы на секунду */
    if (next != null && (!offsetKnown || Math.abs(next - serverOffset) > 1000)) {
      serverOffset = next;
      offsetKnown = true;
    }
  }

  /* Следующий запрос планируется после ответа. На скрытой вкладке опрос замирает
     и возобновляется по visibilitychange. */
  function pollLive() {
    clearTimeout(liveTimer);
    if (document.hidden || liveBusy) return;
    liveBusy = true;
    getJSON('/api/live').then(function (r) {
      updateServerOffset(r);
      if (r.changed) {
        renderLive(liveViewFor(r.data), r.data);
        commitEtag('/api/live', r);
      } else if (state.liveView === 'unavailable' && state.live) {
        renderLive(liveViewFor(state.live), state.live); /* связь с сайтом вернулась, данные те же */
      }
      state.liveOkAt = Date.now();
    }).catch(function () {
      /* Последний ответ держим на экране, пока он не устарел; дальше — «недоступен» */
      var stale = !state.live || Date.now() - state.liveOkAt > LIVE_STALE_AFTER;
      if (stale && state.liveView !== 'unavailable') renderLive('unavailable', state.live);
    }).finally(function () {
      liveBusy = false;
      liveTimer = setTimeout(pollLive, LIVE_EVERY);
    });
  }

  /* ---------- /api/summary + /api/players ---------- */
  var statsTimer = 0, statsBusy = false;

  function pollStats() {
    clearTimeout(statsTimer);
    if (document.hidden || statsBusy) return;
    statsBusy = true;
    var failed = false;
    var summaryReq = getJSON('/api/summary').then(function (r) {
      if (!r.changed) return;
      if (!r.data || !r.data.day || !r.data.all_time) throw new Error('/api/summary: неожиданный ответ');
      var prev = state.summary;
      state.summary = r.data;
      try { renderSummary(true); } catch (e) { state.summary = prev; throw e; }
      commitEtag('/api/summary', r);
    }).catch(function () { failed = true; });
    var playersReq = getJSON('/api/players').then(function (r) {
      if (!r.changed) return;
      if (!r.data || !Array.isArray(r.data.players)) throw new Error('/api/players: неожиданный ответ');
      table.setPlayers(r.data.players);
      state.players = r.data.players;
      commitEtag('/api/players', r);
    }).catch(function () { failed = true; });

    Promise.all([summaryReq, playersReq]).then(function () {
      var s = state.stats;
      if (failed) {
        s.status = 'error';
        s.retryAt = Date.now() + STATS_EVERY;
      } else {
        s.status = 'ok';
        s.updatedAt = Date.now();
      }
      renderIndicator(el.upd, s);
      renderSummary();
      renderPlayersBlock();
      statsBusy = false;
      statsTimer = setTimeout(pollStats, STATS_EVERY);
    });
  }

  function retryNow() { pollStats(); }

  /* ---------- Запуск ---------- */
  renderLive('loading', null);
  renderSummary();
  table.setLoading(10);
  renderIndicator(el.upd, state.stats);

  pollStats();
  pollLive();
  setInterval(tick, 1000);

  document.addEventListener('visibilitychange', function () {
    if (document.hidden) return;
    pollStats();
    pollLive();
  });
})();
