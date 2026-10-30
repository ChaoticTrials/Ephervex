import React from 'react';
import Head from '@docusaurus/Head';
import useBaseUrl from '@docusaurus/useBaseUrl';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';

export default function ArchiveMeta({
  path,
  testing,
}: {
  path: string;
  testing: boolean;
}): React.JSX.Element {
  const { siteConfig } = useDocusaurusContext();
  const local = useBaseUrl(path);
  return (
    <Head>
      <link
        rel="canonical"
        href={new URL(local.replace(/^\//, ''), `${siteConfig.url}/`).toString()}
      />
      {testing && <meta name="robots" content="noindex, nofollow" />}
    </Head>
  );
}
