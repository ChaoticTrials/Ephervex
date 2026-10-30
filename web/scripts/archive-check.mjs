import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const prefix = process.argv[2] ?? '/';
if (!prefix.startsWith('/') || !prefix.endsWith('/'))
  throw new Error('Pass a base URL like / or /preview/');
const build = path.join(root, 'build');
const publication = JSON.parse(
  fs.readFileSync(path.join(root, 'archive/publication.json'), 'utf8'),
);
const archive = JSON.parse(fs.readFileSync(path.join(root, 'archive/archive-data.json'), 'utf8'));
const routes = [
  ['road-to-v1/index.html', 'Road to v1.0.0'],
  ['road-to-v1/forgejo/issues/295/index.html', 'Issue #295'],
  [`road-to-v1/forgejo/pulls/${archive.pulls[0].number}/index.html`, `Pull request #${archive.pulls[0].number}`],
  ['road-to-v1/forgejo/pulls/3/index.html', archive.pulls.some((entry) => entry.number === 3)
    ? 'Pull request #3' : 'Pull request #3 is unavailable'],
];
for (const [relative, expected] of routes) {
  const html = fs.readFileSync(path.join(build, relative), 'utf8');
  if (!html.includes(expected)) throw new Error(`${relative} is missing prerendered content`);
  if (!html.includes(`href=${prefix}road-to-v1/`) && !html.includes(`href="${prefix}road-to-v1/`))
    throw new Error(`${relative} has no base URL aware archive navigation`);
  const canonicalMatch = html.match(/rel=canonical href=(?:"([^"]+)"|([^\s>]+))/);
  const canonical = canonicalMatch?.[1] ?? canonicalMatch?.[2];
  if (!canonical || new URL(canonical).pathname !== `${prefix}${relative.replace('index.html', '')}`)
    throw new Error(`${relative} has an incorrect canonical URL`);
  if (publication.mode === 'testing' && !html.includes('noindex, nofollow'))
    throw new Error(`${relative} exposed test content to indexing`);
  if (publication.mode === 'approved' && html.includes('noindex, nofollow')) {
    const unavailablePull3 = relative === 'road-to-v1/forgejo/pulls/3/index.html'
      && !archive.pulls.some((entry) => entry.number === 3);
    if (!unavailablePull3) throw new Error(`${relative} was not opened for indexing`);
  }
}
for (const [kind, entries] of [['issues', archive.issues], ['pulls', archive.pulls]]) {
  for (const entry of entries) {
    const relative = `road-to-v1/forgejo/${kind}/${entry.number}/index.html`;
    if (!fs.existsSync(path.join(build, relative)))
      throw new Error(`Missing static route: ${relative}`);
    const html = fs.readFileSync(path.join(build, relative), 'utf8');
    if (publication.mode === 'testing' && !html.includes('noindex, nofollow'))
      throw new Error(`Test route is indexable: ${relative}`);
  }
}
if (archive.releases.length) throw new Error('Forgejo releases entered the website dataset');
const discussionHtml = fs.readFileSync(
  path.join(build, 'road-to-v1/forgejo/issues/13/index.html'), 'utf8',
);
if (!discussionHtml.includes('Reduce navigation friction') || !discussionHtml.includes('Touched by'))
  throw new Error('Exported description or comment text was not rendered');
const codeTitleHtml = fs.readFileSync(
  path.join(build, 'road-to-v1/forgejo/issues/424/index.html'), 'utf8',
);
if (!codeTitleHtml.includes('<code>ManagedMutationReport::repaired</code>'))
  throw new Error('Markdown inline code in a historical title was not rendered');
if (fs.existsSync(path.join(build, 'road-to-v1/forgejo/issues/999999/index.html')))
  throw new Error('Unknown issue unexpectedly has a static route');
const sitemap = fs.readFileSync(path.join(build, 'sitemap.xml'), 'utf8');
if (publication.mode === 'testing' && sitemap.includes('/road-to-v1/'))
  throw new Error('Test routes must be absent from sitemap');
if (publication.mode === 'approved' && !sitemap.includes('/road-to-v1/forgejo/issues/295/'))
  throw new Error('Approved routes missing from sitemap');
if (!archive.pulls.some((entry) => entry.number === 3) && sitemap.includes('/road-to-v1/forgejo/pulls/3/'))
  throw new Error('Unavailable PR route must be absent from sitemap');
if (fs.existsSync(path.join(build, 'snapshot.json')))
  throw new Error('Private snapshot copied to build');
console.log(`Archive prerendering, metadata, sitemap, and base URL ${prefix} verified.`);
