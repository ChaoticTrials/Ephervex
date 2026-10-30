import React from 'react';
import Link from '@docusaurus/Link';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import ArchiveMeta from './ArchiveMeta';
import { archiveHomeUrl, archiveIssueUrl, archivePullUrl } from './routes.ts';
import './archive.css';

export default function ArchiveUnavailable({
  unavailable,
}: {
  unavailable: { hasIssue: boolean };
}): React.JSX.Element {
  return (
    <Layout
      title="Pull request #3 unavailable | Road to v1.0.0"
      description="The reviewed Forgejo export contains no pull request #3."
    >
      <ArchiveMeta path={archivePullUrl(3)} testing />
      <main className="archive-root archive-detail-root">
        <div className="container archive-detail-shell">
          <nav aria-label="Archive breadcrumb" className="archive-breadcrumb">
            <Link to={archiveHomeUrl}>Road to v1.0.0</Link>
            <span aria-hidden="true">/</span>
            <span>Pull request #3</span>
          </nav>
          <div className="archive-empty">
            <Heading as="h1">Pull request #3 is unavailable</Heading>
            <p>
              The exported Forgejo history contains no pull request with this number.
              This permanent address is retained for links created during migration.
            </p>
            {unavailable.hasIssue && (
              <p>
                Forgejo issue #3 is available separately: <Link to={archiveIssueUrl(3)}>view issue #3</Link>.
              </p>
            )}
            <p><Link to={archiveHomeUrl}>Explore the archive</Link></p>
          </div>
        </div>
      </main>
    </Layout>
  );
}
