import React, { useMemo, useState } from 'react';
import Link from '@docusaurus/Link';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import type { ArchiveSummary, ExplorerFilters } from './explorer.ts';
import { archiveStats, filterEntries } from './explorer.ts';
import { archiveHomeUrl } from './routes.ts';
import ArchiveLabel from './ArchiveLabel';
import ArchiveMeta from './ArchiveMeta';
import ArchiveTitle from './ArchiveTitle';
import './archive.css';

type Overview = {
  publication: { mode: 'testing' | 'approved' };
  generatedAt: string;
  summaries: ArchiveSummary[];
  labelColors: Record<string, string>;
};
const initial: ExplorerFilters = {
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
const pageSize = 30;
const dateLabel = (date: string | null) =>
  date
    ? new Date(date).toLocaleDateString('en', {
        year: 'numeric', month: 'short', day: 'numeric', timeZone: 'UTC',
      })
    : 'Date unavailable';

export default function ArchiveHome({ overview }: { overview: Overview }): React.JSX.Element {
  const [filters, setFilters] = useState(initial);
  const [page, setPage] = useState(1);
  const testing = overview.publication.mode === 'testing';
  const entries = overview.summaries;
  const stats = useMemo(() => archiveStats(entries), [entries]);
  const matches = useMemo(() => filterEntries(entries, filters), [entries, filters]);
  const totalPages = Math.max(1, Math.ceil(matches.length / pageSize));
  const visible = matches.slice((page - 1) * pageSize, page * pageSize);
  const labels = [...new Set(entries.flatMap((entry) => entry.labels))].sort();
  const milestones = [...new Set(entries
    .map((entry) => entry.milestone)
    .filter((value): value is string => Boolean(value)))].sort();
  const advancedCount = [filters.label, filters.milestone, filters.from, filters.to]
    .filter(Boolean).length;
  const change = <K extends keyof ExplorerFilters>(key: K, value: ExplorerFilters[K]) => {
    setFilters((current) => ({ ...current, [key]: value }));
    setPage(1);
  };
  const selectStat = (selected: Partial<ExplorerFilters>, active: boolean) => {
    setFilters(active ? initial : { ...initial, ...selected });
    setPage(1);
    document.getElementById('explore')?.scrollIntoView();
  };
  const clear = () => {
    setFilters(initial);
    setPage(1);
  };
  return (
    <Layout
      title="Road to v1.0.0 — The Forgejo Era"
      description="Explore the archived issues, pull requests, and discussions that shaped Ephervex before v1.0.0."
    >
      <ArchiveMeta path={archiveHomeUrl} testing={testing} />
      <main className="archive-root">
        <header className="archive-hero">
          <div className="container archive-hero-inner">
            <div className="archive-hero-copy">
              <span className="archive-kicker">EPHERVEX ARCHIVE / THE FORGEJO ERA</span>
              <Heading as="h1">Road to <em>v1.0.0</em></Heading>
              <p>The permanent archive of Ephervex's Forgejo issues and pull requests.</p>
              <a className="archive-hero-link" href="#explore">Explore the records <span aria-hidden="true">→</span></a>
              {testing && (
                <p className="archive-preview" role="status">
                  <strong>Test build.</strong> Real export; publication review pending.
                </p>
              )}
            </div>
            <div className="archive-hero-index">
              <span>THE PRESERVED RECORD</span>
              <strong>{entries.length.toLocaleString('en-US')}</strong>
              <span>issues and pull requests</span>
              <small>Original Forgejo numbers and archive URLs retained.</small>
            </div>
          </div>
        </header>
        <div className="container archive-body">
          <section aria-label="Browse by record type" className="archive-stats-section">
            <div className="archive-stats-intro">
              <span className="archive-kicker">THE RECORD</span>
              <span>Exported {dateLabel(overview.generatedAt)}</span>
            </div>
            <div className="archive-stats">
              <button type="button" aria-pressed={filters.kind === 'issue'}
                onClick={() => selectStat({ kind: 'issue' }, filters.kind === 'issue')}>
                <strong>{stats.issues}</strong><span>Issues</span>
              </button>
              <button type="button" aria-pressed={filters.kind === 'pull' && !filters.status}
                onClick={() => selectStat({ kind: 'pull' }, filters.kind === 'pull' && !filters.status)}>
                <strong>{stats.pulls}</strong><span>Pull requests</span>
              </button>
              <button type="button" aria-pressed={filters.status === 'merged'}
                onClick={() => selectStat({ kind: 'pull', status: 'merged' }, filters.status === 'merged')}>
                <strong>{stats.merged}</strong><span>Merged PRs</span>
              </button>
              <button type="button" aria-pressed={filters.discussed}
                onClick={() => selectStat({ discussed: true }, filters.discussed)}>
                <strong>{stats.discussed}</strong><span>Discussed threads</span>
              </button>
            </div>
          </section>
          <section id="explore" className="archive-explore" aria-labelledby="archive-explore-title">
            <div className="archive-section-heading">
              <div>
                <span className="archive-kicker">FIND A RECORD</span>
                <Heading as="h2" id="archive-explore-title">Browse the archive</Heading>
              </div>
              <p>Search titles, descriptions, comments, and reviews.</p>
            </div>
            <div className="archive-filter-panel">
              <label className="archive-search">
                Search all exported text
                <input type="search" value={filters.query}
                  onChange={(event) => change('query', event.target.value)}
                  placeholder="Search issues and pull requests…" />
              </label>
              <div className="archive-filter-grid">
                <label>Type
                  <select value={filters.kind} onChange={(event) => change('kind', event.target.value)}>
                    <option value="">All types</option><option value="issue">Issues</option><option value="pull">Pull requests</option>
                  </select>
                </label>
                <label>Status
                  <select value={filters.status} onChange={(event) => change('status', event.target.value)}>
                    <option value="">All statuses</option><option value="open">Open</option>
                    <option value="closed">Closed</option><option value="merged">Merged</option>
                  </select>
                </label>
                <label>Sort
                  <select value={filters.sort} onChange={(event) => change('sort', event.target.value)}>
                    <option value="newest">Newest first</option><option value="oldest">Oldest first</option>
                    <option value="updated">Recently updated</option><option value="number">Number</option>
                  </select>
                </label>
                <button type="button" className="archive-reset" onClick={clear}>Clear filters</button>
              </div>
              <details className="archive-advanced">
                <summary>More filters{advancedCount > 0 ? ` (${advancedCount} active)` : ''}</summary>
                <div className="archive-filter-grid">
                  <label>Label
                    <select value={filters.label} onChange={(event) => change('label', event.target.value)}>
                      <option value="">All labels</option>
                      {labels.map((label) => <option key={label}>{label}</option>)}
                    </select>
                  </label>
                  <label>Milestone
                    <select value={filters.milestone} onChange={(event) => change('milestone', event.target.value)}>
                      <option value="">All milestones</option>
                      {milestones.map((milestone) => <option key={milestone}>{milestone}</option>)}
                    </select>
                  </label>
                  <label>From
                    <input type="date" value={filters.from} onChange={(event) => change('from', event.target.value)} />
                  </label>
                  <label>To
                    <input type="date" value={filters.to} onChange={(event) => change('to', event.target.value)} />
                  </label>
                </div>
              </details>
            </div>
            <p className="archive-results" role="status">
              {matches.length} {matches.length === 1 ? 'record' : 'records'} found
              {filters.discussed ? ` · ${stats.comments} comments in the export` : ''}
            </p>
            {visible.length ? (
              <div className="archive-entry-list">
                {visible.map((entry) => (
                  <Link className="archive-entry" to={entry.url} key={entry.url}
                    aria-label={`${entry.kind === 'issue' ? 'Issue' : 'Pull request'} #${entry.number}: ${entry.title}`}>
                    <span className="archive-entry-index" aria-hidden="true">
                      <small>{entry.kind === 'issue' ? 'ISSUE' : 'PR'}</small>
                      <strong>#{entry.number}</strong>
                    </span>
                    <div className="archive-entry-copy">
                      <div className="archive-entry-meta">
                        <span className={`archive-state ${entry.merged ? 'archive-state-merged' : ''}`}>
                          {entry.merged ? 'Merged' : entry.state}
                        </span>
                        <time dateTime={entry.createdAt ?? undefined}>{dateLabel(entry.createdAt)}</time>
                      </div>
                      <h3><ArchiveTitle text={entry.title} /></h3>
                      <div className="archive-entry-footer">
                        <span className="archive-entry-byline">by {entry.author}
                          {entry.commentCount > 0 ? ` · ${entry.commentCount} comments` : ''}
                        </span>
                        <div className="archive-tags" aria-label="Labels and milestone">
                          {entry.milestone && <span className="archive-milestone">{entry.milestone}</span>}
                          {entry.labels.slice(0, 3).map((label) => (
                            <ArchiveLabel key={label} name={label} color={overview.labelColors[label]} />
                          ))}
                          {entry.labels.length > 3 && <span className="archive-more-labels">+{entry.labels.length - 3}</span>}
                        </div>
                      </div>
                    </div>
                    <span className="archive-entry-arrow" aria-hidden="true">→</span>
                  </Link>
                ))}
              </div>
            ) : (
              <div className="archive-empty">
                <strong>{entries.length ? 'No records match those filters.' : 'No records have been exported yet.'}</strong>
                {entries.length > 0 && <p>Try a shorter search or clear a filter.</p>}
                {entries.length > 0 && <button type="button" onClick={clear}>Clear filters</button>}
              </div>
            )}
            {totalPages > 1 && (
              <nav className="archive-pagination" aria-label="Archive result pages">
                <button type="button" disabled={page === 1} onClick={() => setPage(page - 1)}>Previous</button>
                <span>Page {page} of {totalPages}</span>
                <button type="button" disabled={page === totalPages} onClick={() => setPage(page + 1)}>Next</button>
              </nav>
            )}
          </section>
          <aside className="archive-colophon">
            <p>Original Forgejo numbers and archive URLs are preserved. Historical attachments may be unavailable.</p>
            <p>Ephervex continues on <a href="https://github.com/ChaoticTrials/Ephervex">GitHub</a>.</p>
          </aside>
        </div>
      </main>
    </Layout>
  );
}
