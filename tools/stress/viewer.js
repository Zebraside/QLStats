// k6: сценарий «зритель». Запуск:
//   k6 run -e BASE=http://127.0.0.1:8082 -e VUS=300 -e DURATION=5m tools/stress/viewer.js
import http from 'k6/http';
import { check, sleep } from 'k6';
import { Counter } from 'k6/metrics';

const BASE = __ENV.BASE || 'http://127.0.0.1:8082';
const VUS = parseInt(__ENV.VUS || '300');
const DURATION = __ENV.DURATION || '5m';

const notModified = new Counter('responses_304');
const fullBody = new Counter('responses_200');
const bad = new Counter('bad_status');

export const options = {
  scenarios: {
    viewers: { executor: 'constant-vus', vus: VUS, duration: DURATION },
  },
  // 304 считаем успешным ответом
  thresholds: {
    bad_status: ['count==0'],
    http_req_failed: ['rate==0'],
    http_req_duration: ['p(99)<100'],
    'http_req_duration{ep:live}': ['p(99)<100'],
    'http_req_duration{ep:summary}': ['p(99)<100'],
    'http_req_duration{ep:players}': ['p(99)<100'],
  },
  summaryTrendStats: ['avg', 'min', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

http.setResponseCallback(http.expectedStatuses(200, 304));

// ETag по VU
const etags = {};

function get(path, ep) {
  const headers = {};
  if (etags[path]) headers['If-None-Match'] = etags[path];
  const res = http.get(BASE + path, { headers, tags: { ep } });
  const ok = res.status === 200 || res.status === 304;
  if (!ok) bad.add(1);
  if (res.status === 304) notModified.add(1);
  if (res.status === 200) {
    fullBody.add(1);
    const e = res.headers['Etag'] || res.headers['ETag'];
    if (e) etags[path] = e;
  }
  check(res, { 'status 200/304': () => ok });
  return res;
}

export default function () {
  // страница: без ETag при первой загрузке
  get('/', 'page');
  get('/app.js', 'static');
  get('/style.css', 'static');
  get('/api/summary', 'summary');
  get('/api/players', 'players');
  get('/api/live', 'live');

  // случайный сдвиг фазы, чтобы 300 VU не били синхронно
  const start = Date.now();
  const phaseLive = Math.random() * 5000;
  const phaseSlow = Math.random() * 30000;
  let nextLive = start + phaseLive;
  let nextSlow = start + phaseSlow;
  const end = start + 3600 * 1000; // выход по завершению сценария
  while (Date.now() < end) {
    const now = Date.now();
    if (now >= nextLive) { get('/api/live', 'live'); nextLive += 5000; }
    if (now >= nextSlow) {
      get('/api/summary', 'summary');
      get('/api/players', 'players');
      nextSlow += 30000;
    }
    sleep(0.25);
  }
}
