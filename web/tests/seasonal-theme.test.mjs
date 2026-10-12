import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const script = readFileSync(new URL('../static/seasonal-theme.js', import.meta.url), 'utf8');
function season(month, day, search = '', hostname = 'example.com') {
  const document = {documentElement: {dataset: {}}, currentScript: {src: 'https://example.com/preview/seasonal-theme.js'}, addEventListener() {}};
  vm.runInNewContext(script, {Date: class extends Date {getMonth() {return month - 1;} getDate() {return day;}}, location: {hostname, search}, document, URL, URLSearchParams, sessionStorage: {setItem() {}, getItem() {return new URLSearchParams(search).get('seasonPreview');}}});
  return document.documentElement.dataset.season;
}
test('Pride covers all of June and stops at its boundaries', () => {
  assert.equal(season(5, 31), undefined);
  for (let day = 1; day <= 30; day++) assert.equal(season(6, day), 'pride');
  assert.equal(season(7, 1), undefined);
});
test('Halloween remains October 31 only', () => {
  assert.equal(season(10, 30), undefined);
  assert.equal(season(10, 31), 'halloween');
  assert.equal(season(11, 1), undefined);
});
test('previews are restricted to local hosts and can disable a scheduled season', () => {
  assert.equal(season(2, 1, '?seasonPreview=pride'), undefined);
  assert.equal(season(2, 1, '?seasonPreview=pride', 'localhost'), 'pride');
  assert.equal(season(6, 1, '?seasonPreview=off', 'localhost'), undefined);
  assert.equal(season(2, 1, '?halloweenPreview=1', 'localhost'), undefined);
  assert.equal(season(10, 31, '?halloweenPreview=0', 'localhost'), 'halloween');
});
test('branding uses the deployment subpath and updates replaced navigation images', () => {
  const images = [{src: 'default-logo.png'}];
  const favicon = {href: 'default-favicon.png'};
  let ready;
  let update;
  const document = {
    documentElement: {dataset: {}}, currentScript: {src: 'https://example.com/preview/seasonal-theme.js'},
    head: {}, body: {},
    querySelector: () => favicon, querySelectorAll: () => images,
    addEventListener: (_, callback) => {ready = callback;},
  };
  vm.runInNewContext(script, {
    Date: class extends Date {getMonth() {return 5;} getDate() {return 1;}},
    location: {hostname: 'example.com', search: ''}, document, URL, URLSearchParams,
    MutationObserver: class {constructor(callback) {update = callback;} observe() {}},
  });
  ready();
  assert.equal(images[0].src, 'https://example.com/preview/img/pride-logo.png');
  assert.equal(favicon.href, 'https://example.com/preview/img/pride-favicon.png');
  images.push({src: 'default-logo.png'});
  update();
  assert.equal(images[1].src, images[0].src);
});
