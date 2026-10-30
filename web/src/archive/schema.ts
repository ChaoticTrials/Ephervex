import { archiveHomeUrl, archiveIssueUrl, archivePullUrl } from './routes.ts';

export type ArchiveComment = {
  id: number | null;
  author: string;
  createdAt: string | null;
  updatedAt: string | null;
  bodyMarkdown: string;
};
export type ArchiveReview = {
  author: string;
  state: string | null;
  bodyMarkdown: string;
  submittedAt: string | null;
  comments: ArchiveComment[];
};
export type ArchiveEntryBase = {
  number: number;
  title: string;
  state: string;
  author: string;
  createdAt: string | null;
  updatedAt: string | null;
  closedAt: string | null;
  bodyMarkdown: string;
  labels: string[];
  milestone: string | null;
  assignees: string[];
  comments: ArchiveComment[];
};
export type ArchiveIssue = ArchiveEntryBase & { kind: 'issue' };
export type ArchivePull = ArchiveEntryBase & {
  kind: 'pull';
  mergedAt: string | null;
  merged: boolean;
  baseBranch: string | null;
  headBranch: string | null;
  reviews: ArchiveReview[];
};
export type ArchiveEntry = ArchiveIssue | ArchivePull;
export type ArchiveData = {
  schemaVersion: 1;
  archive: string;
  generatedAt: string;
  source: string;
  repository: string;
  routeContract: { home: string; issue: string; pull: string };
  issues: ArchiveIssue[];
  pulls: ArchivePull[];
  milestones: { title: string | null; state: string | null; description: string }[];
  labels: { name: string | null; color: string | null; description: string }[];
  releases: {
    tag: string | null;
    name: string | null;
    createdAt: string | null;
    publishedAt: string | null;
    bodyMarkdown: string;
    draft: boolean | null;
  }[];
};

function object(value: unknown, path: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error(`${path} must be an object`);
  return value as Record<string, unknown>;
}
function string(value: unknown, path: string): string {
  if (typeof value !== 'string') throw new Error(`${path} must be a string`);
  return value;
}
function nullableString(value: unknown, path: string): string | null {
  return value === null ? null : string(value, path);
}
function date(value: unknown, path: string, nullable = true): string | null {
  const result = nullable && value === null ? null : string(value, path);
  if (result !== null && !Number.isFinite(Date.parse(result)))
    throw new Error(`${path} must be a date`);
  return result;
}
function array(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
  return value;
}
function strings(value: unknown, path: string): string[] {
  return array(value, path).map((item, i) => string(item, `${path}[${i}]`));
}
function comment(value: unknown, path: string): ArchiveComment {
  const row = object(value, path);
  if (row.id !== null && (!Number.isSafeInteger(row.id) || Number(row.id) < 0))
    throw new Error(`${path}.id must be a nonnegative integer or null`);
  return {
    id: row.id as number | null,
    author: string(row.author, `${path}.author`),
    createdAt: date(row.createdAt, `${path}.createdAt`),
    updatedAt: date(row.updatedAt, `${path}.updatedAt`),
    bodyMarkdown: string(row.bodyMarkdown, `${path}.bodyMarkdown`),
  };
}
function entry(value: unknown, kind: 'issue' | 'pull', path: string): ArchiveEntry {
  const row = object(value, path);
  if (row.kind !== kind) throw new Error(`${path}.kind must be ${kind}`);
  if (!Number.isSafeInteger(row.number) || Number(row.number) < 1)
    throw new Error(`${path}.number must be a positive integer`);
  const base: ArchiveEntryBase = {
    number: row.number as number,
    title: string(row.title, `${path}.title`),
    state: string(row.state, `${path}.state`),
    author: string(row.author, `${path}.author`),
    createdAt: date(row.createdAt, `${path}.createdAt`),
    updatedAt: date(row.updatedAt, `${path}.updatedAt`),
    closedAt: date(row.closedAt, `${path}.closedAt`),
    bodyMarkdown: string(row.bodyMarkdown, `${path}.bodyMarkdown`),
    labels: strings(row.labels, `${path}.labels`),
    milestone: nullableString(row.milestone, `${path}.milestone`),
    assignees: strings(row.assignees, `${path}.assignees`),
    comments: array(row.comments, `${path}.comments`).map((item, i) =>
      comment(item, `${path}.comments[${i}]`),
    ),
  };
  if (kind === 'issue') return { ...base, kind };
  if (typeof row.merged !== 'boolean') throw new Error(`${path}.merged must be a boolean`);
  return {
    ...base,
    kind,
    merged: row.merged,
    mergedAt: date(row.mergedAt, `${path}.mergedAt`),
    baseBranch: nullableString(row.baseBranch, `${path}.baseBranch`),
    headBranch: nullableString(row.headBranch, `${path}.headBranch`),
    reviews: array(row.reviews, `${path}.reviews`).map((item, i) => {
      const review = object(item, `${path}.reviews[${i}]`);
      return {
        author: string(review.author, `${path}.reviews[${i}].author`),
        state: nullableString(review.state, `${path}.reviews[${i}].state`),
        bodyMarkdown: string(review.bodyMarkdown, `${path}.reviews[${i}].bodyMarkdown`),
        submittedAt: date(review.submittedAt, `${path}.reviews[${i}].submittedAt`),
        comments: array(review.comments, `${path}.reviews[${i}].comments`).map((c, j) =>
          comment(c, `${path}.reviews[${i}].comments[${j}]`),
        ),
      };
    }),
  };
}

export function parseArchiveData(value: unknown): ArchiveData {
  const data = object(value, 'archive');
  if (data.schemaVersion !== 1) throw new Error('Unsupported archive schemaVersion; expected 1');
  const contract = object(data.routeContract, 'routeContract');
  if (
    contract.home !== archiveHomeUrl ||
    contract.issue !== archiveIssueUrl(123).replace('/123/', '/{number}/') ||
    contract.pull !== archivePullUrl(123).replace('/123/', '/{number}/')
  ) {
    throw new Error('Archive route contract differs from permanent routes');
  }
  const issues = array(data.issues, 'issues').map(
    (item, i) => entry(item, 'issue', `issues[${i}]`) as ArchiveIssue,
  );
  const pulls = array(data.pulls, 'pulls').map(
    (item, i) => entry(item, 'pull', `pulls[${i}]`) as ArchivePull,
  );
  const seen = new Set<number>();
  for (const item of [...issues, ...pulls]) {
    if (seen.has(item.number)) throw new Error(`Duplicate Forgejo number #${item.number}`);
    seen.add(item.number);
  }
  return {
    schemaVersion: 1,
    archive: string(data.archive, 'archive.archive'),
    generatedAt: date(data.generatedAt, 'generatedAt', false)!,
    source: string(data.source, 'source'),
    repository: string(data.repository, 'repository'),
    routeContract: {
      home: contract.home as string,
      issue: contract.issue as string,
      pull: contract.pull as string,
    },
    issues,
    pulls,
    milestones: array(data.milestones, 'milestones').map((item, i) => {
      const m = object(item, `milestones[${i}]`);
      return {
        title: nullableString(m.title, `milestones[${i}].title`),
        state: nullableString(m.state, `milestones[${i}].state`),
        description: string(m.description, `milestones[${i}].description`),
      };
    }),
    labels: array(data.labels, 'labels').map((item, i) => {
      const m = object(item, `labels[${i}]`);
      return {
        name: nullableString(m.name, `labels[${i}].name`),
        color: nullableString(m.color, `labels[${i}].color`),
        description: string(m.description, `labels[${i}].description`),
      };
    }),
    releases: array(data.releases, 'releases').map((item, i) => {
      const m = object(item, `releases[${i}]`);
      if (m.draft !== null && typeof m.draft !== 'boolean')
        throw new Error(`releases[${i}].draft must be boolean or null`);
      return {
        tag: nullableString(m.tag, `releases[${i}].tag`),
        name: nullableString(m.name, `releases[${i}].name`),
        createdAt: date(m.createdAt, `releases[${i}].createdAt`),
        publishedAt: date(m.publishedAt, `releases[${i}].publishedAt`),
        bodyMarkdown: string(m.bodyMarkdown, `releases[${i}].bodyMarkdown`),
        draft: m.draft as boolean | null,
      };
    }),
  };
}
