import React from 'react';
import Link from '@docusaurus/Link';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import type { ArchiveEntry, ArchiveComment, ArchiveReview } from './schema.ts';
import { archiveHomeUrl, archiveIssueUrl, archivePullUrl } from './routes.ts';
import ArchiveMarkdown from './ArchiveMarkdown';
import ArchiveLabel from './ArchiveLabel';
import ArchiveMeta from './ArchiveMeta';
import ArchiveTitle from './ArchiveTitle';
import './archive.css';

type Detail = {
  entry: ArchiveEntry;
  publication: { mode: 'testing' | 'approved' };
  siblingNumbers: { issues: number[]; pulls: number[] };
  labelColors: Record<string, string>;
};
const formatDate = (value: string | null) =>
  value
    ? new Date(value).toLocaleString('en', {
        year: 'numeric',
        month: 'long',
        day: 'numeric',
        timeZone: 'UTC',
      })
    : 'Not recorded';

function DiscussionCard({
  comment,
  issues,
  pulls,
}: {
  comment: ArchiveComment;
  issues: number[];
  pulls: number[];
}): React.JSX.Element {
  return (
    <article className="archive-discussion-card">
      <header>
        <div>
          <strong>{comment.author}</strong>
          <time dateTime={comment.createdAt ?? undefined}>{formatDate(comment.createdAt)}</time>
        </div>
      </header>
      <ArchiveMarkdown source={comment.bodyMarkdown} issues={issues} pulls={pulls} />
    </article>
  );
}

function ReviewCard({
  review,
  issues,
  pulls,
}: {
  review: ArchiveReview;
  issues: number[];
  pulls: number[];
}): React.JSX.Element {
  return (
    <article className="archive-discussion-card archive-review">
      <header>
        <div>
          <strong>{review.author}</strong>
          <time dateTime={review.submittedAt ?? undefined}>{formatDate(review.submittedAt)}</time>
        </div>
        <span className="archive-review-state">{review.state ?? 'Review'}</span>
      </header>
      <ArchiveMarkdown source={review.bodyMarkdown} issues={issues} pulls={pulls} />
      {review.comments.length > 0 && (
        <div className="archive-review-comments">
          <strong>Review discussion</strong>
          {review.comments.map((comment, index) => (
            <DiscussionCard
              key={comment.id ?? index}
              comment={comment}
              issues={issues}
              pulls={pulls}
            />
          ))}
        </div>
      )}
    </article>
  );
}

export default function ArchiveDetail({ detail }: { detail: Detail }): React.JSX.Element {
  const { entry, publication, siblingNumbers, labelColors } = detail;
  const testing = publication.mode === 'testing';
  const path =
    entry.kind === 'issue' ? archiveIssueUrl(entry.number) : archivePullUrl(entry.number);
  const kind = entry.kind === 'issue' ? 'Issue' : 'Pull request';
  const state = entry.kind === 'pull' && entry.merged ? 'Merged' : entry.state;
  return (
    <Layout
      title={`${kind} #${entry.number}: ${entry.title} | Road to v1.0.0`}
      description={`${kind} #${entry.number}: ${entry.title}. Archived Forgejo discussion from Ephervex's road to v1.0.0.`}
    >
      <ArchiveMeta path={path} testing={testing} />
      <main className="archive-root archive-detail-root">
        <div className="container archive-detail-shell">
          <nav aria-label="Archive breadcrumb" className="archive-breadcrumb">
            <Link to={archiveHomeUrl}>Road to v1.0.0</Link>
            <span aria-hidden="true">/</span>
            <span>
              {kind} #{entry.number}
            </span>
          </nav>
          {testing && (
            <p className="archive-preview" role="status">
              <strong>Live archive test build.</strong> This is a real exported record.
              Publication review and deployment are still pending.
            </p>
          )}
          <header className="archive-detail-head">
            <span className="archive-kicker">
              FORGEJO ERA / {entry.kind === 'issue' ? 'ISSUE' : 'PULL REQUEST'} #{entry.number}
            </span>
            <Heading as="h1"><ArchiveTitle text={entry.title} /></Heading>
            <div className="archive-detail-headline">
              <span className={`archive-state ${state === 'Merged' ? 'archive-state-merged' : ''}`}>
                {state}
              </span>
              <span>
                Opened by <strong>{entry.author}</strong> on{' '}
                <time dateTime={entry.createdAt ?? undefined}>{formatDate(entry.createdAt)}</time>
              </span>
            </div>
          </header>
          <div className="archive-detail-grid">
            <div className="archive-detail-main">
              <section className="archive-description" aria-labelledby="archive-description-title">
                <div className="archive-content-heading">
                  <div>
                    <Heading as="h2" id="archive-description-title">
                      Original description
                    </Heading>
                    <small>
                      By {entry.author} · {formatDate(entry.createdAt)}
                    </small>
                  </div>
                </div>
                {entry.bodyMarkdown.trim() ? (
                  <ArchiveMarkdown
                    source={entry.bodyMarkdown}
                    issues={siblingNumbers.issues}
                    pulls={siblingNumbers.pulls}
                  />
                ) : (
                  <p className="archive-empty">No description was included in this export.</p>
                )}
              </section>
              <section className="archive-discussion" aria-labelledby="archive-discussion-title">
                <div className="archive-section-heading">
                  <div>
                    <span className="archive-kicker">THE CONVERSATION</span>
                    <Heading as="h2" id="archive-discussion-title">
                      Discussion history
                    </Heading>
                  </div>
                  <p>
                    {entry.comments.length} {entry.comments.length === 1 ? 'comment' : 'comments'}
                  </p>
                </div>
                {entry.comments.length ? (
                  entry.comments.map((comment, index) => (
                    <DiscussionCard
                      key={comment.id ?? index}
                      comment={comment}
                      issues={siblingNumbers.issues}
                      pulls={siblingNumbers.pulls}
                    />
                  ))
                ) : (
                  <p className="archive-empty">No comments were included in this public export.</p>
                )}
              </section>
              {entry.kind === 'pull' && (
                <section className="archive-discussion" aria-labelledby="archive-reviews-title">
                  <div className="archive-section-heading">
                    <div>
                      <span className="archive-kicker">REVIEW RECORD</span>
                      <Heading as="h2" id="archive-reviews-title">
                        Reviews
                      </Heading>
                    </div>
                    <p>
                      {entry.reviews.length} {entry.reviews.length === 1 ? 'review' : 'reviews'}
                    </p>
                  </div>
                  {entry.reviews.length ? (
                    entry.reviews.map((review, index) => (
                      <ReviewCard
                        key={`${review.author}-${index}`}
                        review={review}
                        issues={siblingNumbers.issues}
                        pulls={siblingNumbers.pulls}
                      />
                    ))
                  ) : (
                    <p className="archive-empty">No reviews were included in this public export.</p>
                  )}
                </section>
              )}
            </div>
            <aside className="archive-detail-aside" aria-label="Entry details">
              <div className="archive-aside-block">
                <h2>Record details</h2>
                <dl>
                  <div>
                    <dt>Status</dt>
                    <dd>{state}</dd>
                  </div>
                  <div>
                    <dt>Created</dt>
                    <dd>{formatDate(entry.createdAt)}</dd>
                  </div>
                  <div>
                    <dt>Updated</dt>
                    <dd>{formatDate(entry.updatedAt)}</dd>
                  </div>
                  <div>
                    <dt>Closed</dt>
                    <dd>{formatDate(entry.closedAt)}</dd>
                  </div>
                  {entry.kind === 'pull' && (
                    <>
                      <div>
                        <dt>Merged</dt>
                        <dd>{formatDate(entry.mergedAt)}</dd>
                      </div>
                      <div>
                        <dt>Branches</dt>
                        <dd>
                          {entry.headBranch ?? 'Unknown'} → {entry.baseBranch ?? 'Unknown'}
                        </dd>
                      </div>
                    </>
                  )}
                </dl>
              </div>
              <div className="archive-aside-block">
                <h2>Milestone</h2>
                <p>{entry.milestone ?? 'None recorded'}</p>
              </div>
              <div className="archive-aside-block">
                <h2>Labels</h2>
                {entry.labels.length ? (
                  <div className="archive-tags">
                    {entry.labels.map((label) => (
                      <ArchiveLabel key={label} name={label} color={labelColors[label]} />
                    ))}
                  </div>
                ) : (
                  <p>None recorded</p>
                )}
              </div>
              <div className="archive-aside-block">
                <h2>Assignees</h2>
                <p>{entry.assignees.length ? entry.assignees.join(', ') : 'None recorded'}</p>
              </div>
            </aside>
          </div>
          <div className="archive-detail-footer">
            <Link to={archiveHomeUrl}>← Back to the archive</Link>
            <span>Permanent archive link · #{entry.number}</span>
          </div>
        </div>
      </main>
    </Layout>
  );
}
