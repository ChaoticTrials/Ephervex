import React from 'react';
import Link from '@docusaurus/Link';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { safeArchiveUrl, linkArchiveReferences } from './markdown.ts';

export default function ArchiveMarkdown({
  source,
  issues,
  pulls,
}: {
  source: string;
  issues: number[];
  pulls: number[];
}): React.JSX.Element {
  if (!source.trim())
    return <p className="archive-muted">No description was included in the public export.</p>;
  return (
    <div className="archive-markdown">
      <ReactMarkdown
        skipHtml
        urlTransform={safeArchiveUrl}
        remarkPlugins={[remarkGfm, linkArchiveReferences(issues, pulls)]}
        components={{
          a({ href, children }) {
            if (!href)
              return (
                <span className="archive-unavailable-link">
                  {children}
                  <span className="archive-sr-only">
                    {' '}
                    (link unavailable in this public archive)
                  </span>
                </span>
              );
            if (href.startsWith('/road-to-v1/')) return <Link to={href}>{children}</Link>;
            return (
              <a href={href} rel="nofollow noopener noreferrer" target="_blank">
                {children}
                <span className="archive-sr-only"> (opens in a new tab)</span>
              </a>
            );
          },
          img({ alt }) {
            return (
              <span className="archive-missing-asset" role="note">
                Image omitted from public export{alt ? `: ${alt}` : '.'}
              </span>
            );
          },
        }}
      >
        {source}
      </ReactMarkdown>
    </div>
  );
}
