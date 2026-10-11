import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import type { LoadContext, Plugin } from '@docusaurus/types';
import { parseArchiveData, type ArchiveData } from '../../src/archive/schema.ts';
import { toSummaries } from '../../src/archive/explorer.ts';
import {
  archiveHomeUrl,
  archiveIssueUrl,
  archivePullUrl,
  archiveRoutePath,
} from '../../src/archive/routes.ts';

type Publication = { mode: 'testing' | 'approved'; sha256: string };
type Content = { data: ArchiveData; publication: Publication };

function forbidPrivateSnapshots(root: string): void {
  for (const dirent of fs.readdirSync(root, { withFileTypes: true })) {
    if (/^snapshot(?:\.backup\.[^.]+)?\.json$/i.test(dirent.name))
      throw new Error(`Private snapshot found in web/: ${path.join(root, dirent.name)}`);
  }
  for (const folder of ['archive', 'src', 'static', 'docs', 'plugins', 'scripts', 'build']) {
    const start = path.join(root, folder);
    if (!fs.existsSync(start)) continue;
    const walk = (directory: string) => {
      for (const dirent of fs.readdirSync(directory, { withFileTypes: true })) {
        if (/^snapshot(?:\.backup\.[^.]+)?\.json$/i.test(dirent.name))
          throw new Error(`Private snapshot found in web/: ${path.join(directory, dirent.name)}`);
        if (dirent.isDirectory()) walk(path.join(directory, dirent.name));
      }
    };
    walk(start);
  }
}

export function loadArchive(root: string): Content {
  forbidPrivateSnapshots(root);
  const file = path.join(root, 'archive/archive-data.json');
  const bytes = fs.readFileSync(file);
  const publication = JSON.parse(
    fs.readFileSync(path.join(root, 'archive/publication.json'), 'utf8'),
  ) as Publication;
  if (
    !['testing', 'approved'].includes(publication.mode) ||
    !/^[a-f0-9]{64}$/.test(publication.sha256)
  )
    throw new Error('Invalid archive publication metadata');
  const actual = createHash('sha256').update(bytes).digest('hex');
  if (actual !== publication.sha256)
    throw new Error('Archive data changed without matching reviewed SHA-256 in publication.json');
  const data = parseArchiveData(JSON.parse(bytes.toString('utf8')));
  if (data.releases.length)
    throw new Error('Forgejo releases must be excluded from the website export');
  return { data, publication };
}

export default function roadToV1(context: LoadContext): Plugin<unknown> {
  return {
    name: 'ephervex-road-to-v1',
    async loadContent() {
      return loadArchive(context.siteDir);
    },
    async contentLoaded({ content, actions }) {
      const { createData, addRoute } = actions;
      const { data, publication } = content as Content;
      const summaries = toSummaries(data);
      const labelColors = Object.fromEntries(
        data.labels
          .filter((label) => label.name && label.color && /^[a-f\d]{6}$/i.test(label.color))
          .map((label) => [label.name, `#${label.color}`]),
      );
      const overview = await createData(
        'road-to-v1-overview.json',
        JSON.stringify({
          publication,
          archive: data.archive,
          generatedAt: data.generatedAt,
          summaries,
          labelColors,
        }),
      );
      addRoute({
        path: archiveRoutePath(archiveHomeUrl, context.siteConfig.baseUrl),
        exact: true,
        component: path.resolve(context.siteDir, 'src/archive/ArchiveHome.tsx'),
        modules: { overview },
      });
      const siblingNumbers = {
        issues: data.issues.map((item) => item.number),
        pulls: data.pulls.map((item) => item.number),
      };
      for (const entry of [...data.issues, ...data.pulls]) {
        const permalink =
          entry.kind === 'issue' ? archiveIssueUrl(entry.number) : archivePullUrl(entry.number);
        const detail = await createData(
          `${entry.kind}-${entry.number}.json`,
          JSON.stringify({
            entry,
            publication,
            siblingNumbers,
            labelColors: Object.fromEntries(entry.labels
              .filter((label) => labelColors[label])
              .map((label) => [label, labelColors[label]])),
          }),
        );
        addRoute({
          path: archiveRoutePath(permalink, context.siteConfig.baseUrl),
          exact: true,
          component: path.resolve(context.siteDir, 'src/archive/ArchiveDetail.tsx'),
          modules: { detail },
        });
      }
      // This URL was named in the permanent migration contract. The live export
      // has issue #3 but no PR #3, so preserve the URL without inventing a PR.
      if (!data.pulls.some((entry) => entry.number === 3)) {
        const unavailable = await createData(
          'pull-3-unavailable.json',
          JSON.stringify({ hasIssue: data.issues.some((entry) => entry.number === 3) }),
        );
        addRoute({
          path: archiveRoutePath(archivePullUrl(3), context.siteConfig.baseUrl),
          exact: true,
          component: path.resolve(context.siteDir, 'src/archive/ArchiveUnavailable.tsx'),
          modules: { unavailable },
        });
      }
    },
  };
}
