import React from 'react';
import ReactMarkdown from 'react-markdown';

export default function ArchiveTitle({ text }: { text: string }): React.JSX.Element {
  return (
    <ReactMarkdown
      skipHtml
      allowedElements={['p', 'em', 'strong', 'code']}
      unwrapDisallowed
      components={{ p: ({ children }) => <>{children}</> }}
    >
      {text}
    </ReactMarkdown>
  );
}
