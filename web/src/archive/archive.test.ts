import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import React from 'react';
import { parseArchiveData } from './schema.ts';
import { archiveHomeUrl, archiveIssueUrl, archivePullUrl, archiveRoutePath } from './routes.ts';
import {
  toSummaries,
  archiveStats,
  filterEntries,
  type ExplorerFilters,
} from './explorer.ts';
import { linkArchiveReferences, safeArchiveUrl } from './markdown.ts';
import roadToV1, { loadArchive } from '../../plugins/road-to-v1/index.ts';
import ArchiveTitle from './ArchiveTitle.tsx';
const renderToStaticMarkup = require('react-dom/server').renderToStaticMarkup as
  (element: React.ReactElement) => string;

const siteDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const fixtureBytes = fs.readFileSync(path.join(siteDir, 'archive/archive-data.json'));
const raw = JSON.parse(fixtureBytes.toString('utf8'));
const data = parseArchiveData(raw);
const summaries = toSummaries(data);
const filters: ExplorerFilters = {
  query: '',
  kind: '',
  status: '',
  label: '',
  milestone: '',
  discussed: false,
  from: '',
  to: '',
  sort: 'newest',
};

test('permanent route contract and schema v1 live export', () => {
  assert.equal(archiveHomeUrl, '/road-to-v1/');
  assert.equal(archiveIssueUrl(295), '/road-to-v1/forgejo/issues/295/');
  assert.equal(archivePullUrl(3), '/road-to-v1/forgejo/pulls/3/');
  assert.equal(
    archiveRoutePath(archiveIssueUrl(295), '/preview/'),
    '/preview/road-to-v1/forgejo/issues/295/',
  );
  assert.throws(() => archiveIssueUrl(0));
  assert.throws(() => parseArchiveData({ ...raw, schemaVersion: 2 }), /schemaVersion/);
  assert.throws(
    () => parseArchiveData({ ...raw, issues: [...raw.issues, raw.issues[0]] }),
    /Duplicate/,
  );
  assert.throws(
    () => parseArchiveData({ ...raw, routeContract: { ...raw.routeContract, issue: '/changed/' } }),
    /route contract/,
  );
  assert.deepEqual(parseArchiveData({ ...raw, issues: [], pulls: [] }).issues, []);
});

test('search, filters, dates, sorting, and real derived counts', () => {
  assert.equal(archiveStats(summaries).issues, data.issues.length);
  assert.equal(archiveStats(summaries).pulls, data.pulls.length);
  assert.ok(filterEntries(summaries, { ...filters, query: '295' }).some(
    (entry) => entry.kind === 'issue' && entry.number === 295,
  ));
  const records = [
    {
      ...summaries.find((entry) => entry.kind === 'issue')!,
      number: 295,
      createdAt: '2026-10-02T12:00:00Z',
      title: 'Encoding fix',
      searchText: 'encoding fix comment',
      labels: ['bug'],
      milestone: 'Roadmap',
      state: 'closed',
      merged: false,
      commentCount: 1,
    },
    {
      ...summaries.find((entry) => entry.kind === 'pull')!,
      number: 3,
      createdAt: '2026-09-29T09:00:00Z',
      title: 'Documentation',
      searchText: 'documentation review',
      labels: ['documentation'],
      milestone: null,
      state: 'closed',
      merged: true,
      commentCount: 0,
    },
  ];
  assert.deepEqual(
    filterEntries(records, { ...filters, query: 'encoding comment' }).map((entry) => entry.number),
    [295],
  );
  assert.deepEqual(
    filterEntries(records, { ...filters, kind: 'pull', status: 'merged' }).map(
      (entry) => entry.number,
    ),
    [3],
  );
  assert.deepEqual(
    filterEntries(records, { ...filters, label: 'documentation' }).map((entry) => entry.number),
    [3],
  );
  assert.deepEqual(
    filterEntries(records, { ...filters, milestone: 'Roadmap' }).map(
      (entry) => entry.number,
    ),
    [295],
  );
  assert.deepEqual(
    filterEntries(records, { ...filters, from: '2026-10-01', to: '2026-10-03' }).map(
      (entry) => entry.number,
    ),
    [295],
  );
  assert.deepEqual(
    filterEntries(records, { ...filters, sort: 'number' }).map((entry) => entry.number),
    [3, 295],
  );
  assert.deepEqual(filterEntries([], filters), []);
  assert.deepEqual(filterEntries(records, { ...filters, discussed: true }).map((entry) => entry.number), [295]);
  assert.equal(archiveStats(records).discussed, 1);
  assert.deepEqual(
    filterEntries([{ ...records[0], createdAt: '2026-10-02T00:30:00+02:00' }], {
      ...filters,
      from: '2026-10-01',
      to: '2026-10-01',
    }).map((entry) => entry.number),
    [295],
  );
});

test('inline code in historical titles renders safely', () => {
  const title = renderToStaticMarkup(React.createElement(ArchiveTitle, {
    text: '`ManagedMutationReport::repaired` docs <script>alert(1)</script>',
  }));
  assert.match(title, /<code>ManagedMutationReport::repaired<\/code>/);
  assert.doesNotMatch(title, /`/);
  assert.doesNotMatch(title, /<script>/);
});

test('untrusted URLs are constrained and only known number references link', () => {
  for (const value of [
    'javascript:alert(1)',
    'data:text/html,test',
    '//evil.example',
    'https://forgejo.lan/private',
    '../attachment.png',
    '/attachments/1',
  ])
    assert.equal(safeArchiveUrl(value), '');
  assert.equal(safeArchiveUrl('https://example.org/a'), 'https://example.org/a');
  assert.equal(safeArchiveUrl('/road-to-v1/'), '/road-to-v1/');
  const tree: {
    type: string;
    children: {
      type: string;
      url?: string;
      children?: { type: string; value?: string; url?: string }[];
    }[];
  } = {
    type: 'root',
    children: [
      { type: 'paragraph', children: [{ type: 'text', value: 'See #295, #3, and #999.' }] },
      { type: 'link', url: 'https://example.org', children: [{ type: 'text', value: '#295' }] },
    ],
  };
  linkArchiveReferences([295], [3])()(tree);
  assert.deepEqual(
    tree.children[0].children?.filter((node) => node.type === 'link').map((node) => node.url),
    [archiveIssueUrl(295), archivePullUrl(3)],
  );
  assert.equal(tree.children[1].children?.[0].value, '#295');
});

test('plugin creates direct static routes and separate data modules', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ephervex-archive-test-'));
  fs.mkdirSync(path.join(root, 'archive'));
  fs.writeFileSync(path.join(root, 'archive/archive-data.json'), fixtureBytes);
  fs.writeFileSync(
    path.join(root, 'archive/publication.json'),
    JSON.stringify({
      mode: 'testing',
      sha256: createHash('sha256').update(fixtureBytes).digest('hex'),
    }),
  );
  assert.equal(loadArchive(root).publication.mode, 'testing');
  const plugin = roadToV1({ siteDir: root, siteConfig: { baseUrl: '/' } } as Parameters<
    typeof roadToV1
  >[0]);
  const content = await plugin.loadContent!();
  const routes: { path: string; modules: Record<string, string> }[] = [];
  const files = new Map<string, string>();
  await plugin.contentLoaded!({
    content,
    actions: {
      createData: async (name: string, value: string) => {
        files.set(name, value);
        return name;
      },
      addRoute: (route: { path: string; modules: Record<string, string> }) => {
        routes.push(route);
      },
      setGlobalData: () => {},
    },
  } as unknown as Parameters<NonNullable<typeof plugin.contentLoaded>>[0]);
  assert.equal(routes.length, 2 + data.issues.length + data.pulls.length);
  assert.ok(routes.some((route) => route.path === archiveHomeUrl));
  assert.ok(routes.some((route) => route.path === archiveIssueUrl(295)));
  assert.ok(routes.some((route) => route.path === archivePullUrl(3)));
  assert.equal(JSON.parse(files.get('issue-295.json')!).entry.number, 295);
  assert.equal(JSON.parse(files.get('pull-1.json')!).entry.number, 1);
  assert.equal(JSON.parse(files.get('pull-3-unavailable.json')!).hasIssue, true);
  assert.equal(JSON.parse(files.get('road-to-v1-overview.json')!).summaries.length, summaries.length);
  assert.ok(!files.has('issue-999999.json'));
  assert.deepEqual(Object.keys(JSON.parse(files.get('issue-295.json')!)).sort(), [
    'entry',
    'labelColors',
    'publication',
    'siblingNumbers',
  ]);
  assert.equal(JSON.parse(files.get('road-to-v1-overview.json')!).labelColors.bug, '#ee0701');
  const subpathPlugin = roadToV1({
    siteDir: root,
    siteConfig: { baseUrl: '/preview/' },
  } as Parameters<typeof roadToV1>[0]);
  const subpathRoutes: string[] = [];
  await subpathPlugin.contentLoaded!({
    content: await subpathPlugin.loadContent!(),
    actions: {
      createData: async (name: string) => name,
      addRoute: (route: { path: string }) => {
        subpathRoutes.push(route.path);
      },
      setGlobalData: () => {},
    },
  } as unknown as Parameters<NonNullable<typeof subpathPlugin.contentLoaded>>[0]);
  assert.equal(subpathRoutes.length, routes.length);
  assert.ok(subpathRoutes.includes('/preview/road-to-v1/'));
  assert.ok(subpathRoutes.includes('/preview/road-to-v1/forgejo/issues/295/'));
  assert.ok(subpathRoutes.includes('/preview/road-to-v1/forgejo/pulls/3/'));
  assert.ok(subpathRoutes.every((route) => route.startsWith('/preview/road-to-v1/')));
  const withRelease = Buffer.from(JSON.stringify({
    ...raw,
    releases: [{
      tag: 'v0.1.0', name: 'Legacy release', createdAt: null,
      publishedAt: null, bodyMarkdown: 'Legacy notes', draft: false,
    }],
  }));
  fs.writeFileSync(path.join(root, 'archive/archive-data.json'), withRelease);
  fs.writeFileSync(path.join(root, 'archive/publication.json'), JSON.stringify({
    mode: 'testing',
    sha256: createHash('sha256').update(withRelease).digest('hex'),
  }));
  assert.throws(() => loadArchive(root), /Forgejo releases/);
  fs.writeFileSync(path.join(root, 'archive/archive-data.json'), '{}');
  assert.throws(() => loadArchive(root), /SHA-256/);
  fs.writeFileSync(path.join(root, 'archive/archive-data.json'), fixtureBytes);
  fs.writeFileSync(path.join(root, 'archive/publication.json'), JSON.stringify({
    mode: 'testing',
    sha256: createHash('sha256').update(fixtureBytes).digest('hex'),
  }));
  fs.writeFileSync(path.join(root, 'archive/snapshot.json'), '{}');
  assert.throws(() => loadArchive(root), /Private snapshot/);
  fs.rmSync(root, { recursive: true, force: true });
});
