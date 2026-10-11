import type { ArchiveData, ArchiveEntry } from './schema.ts';
import { archiveIssueUrl, archivePullUrl } from './routes.ts';

export type ArchiveSummary = {
  kind: 'issue' | 'pull';
  number: number;
  title: string;
  state: string;
  author: string;
  createdAt: string | null;
  updatedAt: string | null;
  closedAt: string | null;
  merged: boolean;
  labels: string[];
  milestone: string | null;
  commentCount: number;
  searchText: string;
  url: string;
};
export type ExplorerFilters = {
  query: string;
  kind: string;
  status: string;
  label: string;
  milestone: string;
  discussed: boolean;
  from: string;
  to: string;
  sort: string;
};

const utcDay = (value: string) => new Date(value).toISOString().slice(0, 10);
const time = (value: string | null) => (value ? Date.parse(value) : 0);

export function toSummaries(data: ArchiveData): ArchiveSummary[] {
  return [...data.issues, ...data.pulls].map((entry: ArchiveEntry) => ({
    kind: entry.kind,
    number: entry.number,
    title: entry.title,
    state: entry.state,
    author: entry.author,
    createdAt: entry.createdAt,
    updatedAt: entry.updatedAt,
    closedAt: entry.closedAt,
    merged: entry.kind === 'pull' && entry.merged,
    labels: entry.labels,
    milestone: entry.milestone,
    commentCount:
      entry.comments.length +
      (entry.kind === 'pull'
        ? entry.reviews.reduce((n, review) => n + review.comments.length, 0)
        : 0),
    searchText: [
      entry.title,
      entry.author,
      entry.bodyMarkdown,
      entry.number,
      ...entry.labels,
      entry.milestone ?? '',
      ...entry.comments.map((comment) => `${comment.author} ${comment.bodyMarkdown}`),
      ...(entry.kind === 'pull'
        ? entry.reviews.flatMap((review) => [
            review.author,
            review.bodyMarkdown,
            ...review.comments.map((comment) => `${comment.author} ${comment.bodyMarkdown}`),
          ])
        : []),
    ]
      .join(' ')
      .toLocaleLowerCase(),
    url: entry.kind === 'issue' ? archiveIssueUrl(entry.number) : archivePullUrl(entry.number),
  }));
}

export function filterEntries(
  entries: ArchiveSummary[],
  filters: ExplorerFilters,
): ArchiveSummary[] {
  const words = filters.query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  return entries
    .filter(
      (entry) =>
        (!filters.kind || entry.kind === filters.kind) &&
        (!filters.status ||
          (filters.status === 'merged'
            ? entry.merged
            : filters.status === 'open'
              ? entry.state === 'open'
              : entry.state === filters.status && !entry.merged)) &&
        (!filters.label || entry.labels.includes(filters.label)) &&
        (!filters.milestone || entry.milestone === filters.milestone) &&
        (!filters.discussed || entry.commentCount > 0) &&
        (!filters.from || (entry.createdAt !== null && utcDay(entry.createdAt) >= filters.from)) &&
        (!filters.to || (entry.createdAt !== null && utcDay(entry.createdAt) <= filters.to)) &&
        words.every((word) => entry.searchText.includes(word)),
    )
    .sort((a, b) => {
      if (filters.sort === 'number') return a.number - b.number;
      if (filters.sort === 'oldest') return time(a.createdAt) - time(b.createdAt);
      if (filters.sort === 'updated') return time(b.updatedAt) - time(a.updatedAt);
      return time(b.createdAt) - time(a.createdAt);
    });
}

export function archiveStats(entries: ArchiveSummary[]) {
  return {
    issues: entries.filter((entry) => entry.kind === 'issue').length,
    pulls: entries.filter((entry) => entry.kind === 'pull').length,
    closed: entries.filter((entry) => entry.state === 'closed' && !entry.merged).length,
    merged: entries.filter((entry) => entry.merged).length,
    comments: entries.reduce((n, entry) => n + entry.commentCount, 0),
    discussed: entries.filter((entry) => entry.commentCount > 0).length,
  };
}
