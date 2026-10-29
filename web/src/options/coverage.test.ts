import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { buildCoverage, parseOptionsDataset, type OptionsDataset } from './coverage.ts';
import { optionsDatasetPath } from '../../plugins/minecraft-options/index.ts';

const siteDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

const small: OptionsDataset = {
  format_version: 1,
  revision: 7,
  canonical_minecraft_version: '1.3',
  setting_registry: [
    { id: 'fov', key: 'fov', type: 'integer', category: 'Video', manageable: true },
    { id: 'old', key: 'oldKey', type: 'boolean', category: 'Video', manageable: true },
    { id: 'misc', key: 'misc', type: 'boolean', category: null, manageable: false },
  ],
  current_canonical_setting_ids: ['fov', 'misc'],
  schemas: [
    { id: 'a', supported: { fov: 0, old: 1 }, unsupported: ['misc'] },
    { id: 'b', supported: { fov: 0, misc: 2 }, unsupported: ['old'] },
  ],
  releases: [
    { minecraft_version: '1.1', schema: 'a', data_version: null },
    { minecraft_version: '1.2', schema: 'a', data_version: 10 },
    { minecraft_version: '1.3', schema: 'b', data_version: 11 },
  ],
};

test('a setting that left the game reports the last release that stored it', () => {
  const coverage = buildCoverage(small);
  const video = coverage.categories.find((category) => category.name === 'Video');
  const old = video?.settings.find((setting) => setting.id === 'old');
  assert.equal(old?.firstRelease, '1.1');
  assert.equal(old?.lastRelease, '1.2');
  assert.equal(old?.current, false);
});

test('a setting added later reports its first release and stays current', () => {
  const coverage = buildCoverage(small);
  const other = coverage.categories.find((category) => category.name === 'Other');
  assert.equal(other?.settings[0]?.firstRelease, '1.3');
  assert.equal(other?.settings[0]?.current, true);
});

test('uncategorized settings are listed last', () => {
  const names = buildCoverage(small).categories.map((category) => category.name);
  assert.deepEqual(names, ['Video', 'Other']);
});

test('release rows keep the dataset order and count each schema entry', () => {
  const releases = buildCoverage(small).releases;
  assert.deepEqual(releases.map((release) => release.version), ['1.1', '1.2', '1.3']);
  assert.deepEqual(releases.map((release) => release.settings), [2, 2, 2]);
  assert.equal(releases[0]?.dataVersion, null);
});

test('a release naming an unknown schema is refused', () => {
  const broken = structuredClone(small);
  broken.releases[0]!.schema = 'missing';
  assert.throws(() => parseOptionsDataset(broken), /unknown schema/);
});

test('an unsupported format version is refused', () => {
  assert.throws(() => parseOptionsDataset({ ...small, format_version: 2 }), /Unsupported/);
});

test('the committed dataset produces a row for every release and setting', () => {
  const raw = JSON.parse(fs.readFileSync(optionsDatasetPath(siteDir), 'utf8'));
  const dataset = parseOptionsDataset(raw);
  const coverage = buildCoverage(dataset);
  assert.equal(coverage.releases.length, dataset.releases.length);
  const listed = coverage.categories.reduce((count, category) => count + category.settings.length, 0);
  assert.equal(listed, dataset.setting_registry.length);
  assert.ok(coverage.releases.every((release) => release.settings > 0));
  assert.equal(coverage.releases.at(-1)?.version, dataset.canonical_minecraft_version);
});

test('a dataset without releases is refused', () => {
  assert.throws(() => parseOptionsDataset({ ...small, releases: [] }), /no releases/);
});
