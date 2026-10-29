import React from 'react';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import useBaseUrl from '@docusaurus/useBaseUrl';
import type { Coverage } from './coverage.ts';
import styles from './options.module.css';

export default function OptionsCoverage({ coverage }: { coverage: Coverage }): React.JSX.Element {
  const feedUrl = useBaseUrl('/assets/minecraft-options.json');
  const settings = coverage.categories.reduce((count, category) => count + category.settings.length, 0);
  const first = coverage.releases[0]?.version;
  const last = coverage.releases[coverage.releases.length - 1]?.version;
  return (
    <Layout
      title="Minecraft options coverage"
      description="The Minecraft releases and options.txt settings in the data Ephervex uses to write preferences."
    >
      <main className={styles.page}>
        <Heading as="h1">Minecraft options coverage</Heading>
        <p>
          Ephervex writes a managed preference into a new instance's <code>options.txt</code> only
          for a Minecraft release listed here, using that release's own key and value format. A
          release that is not listed stays fully usable, but Ephervex writes no managed
          preferences into it. The data was extracted from the official client JARs and is
          published as <a href={feedUrl}>one JSON file</a> that the application reads and checks
          before using.
        </p>
        <dl className={styles.summary}>
          <div><dt>Releases</dt><dd>{coverage.releases.length}</dd></div>
          <div><dt>Range</dt><dd>{first} to {last}</dd></div>
          <div><dt>Settings</dt><dd>{settings}</dd></div>
          <div><dt>Revision</dt><dd>{coverage.revision}</dd></div>
        </dl>

        <Heading as="h2" id="releases">Releases</Heading>
        <p>
          Each release lists how many of the settings below it stores. The data version is the
          number the release writes into its own <code>options.txt</code>.
        </p>
        <div className={styles.scroll}>
          <table className={styles.table}>
            <thead>
              <tr><th>Minecraft</th><th>Data version</th><th>Settings</th></tr>
            </thead>
            <tbody>
              {coverage.releases.map((release) => (
                <tr key={release.version}>
                  <td><code>{release.version}</code></td>
                  <td>{release.dataVersion ?? 'Not recorded'}</td>
                  <td>{release.settings}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <Heading as="h2" id="settings">Settings</Heading>
        <p>
          Every setting the data describes, named as in release {coverage.canonicalVersion}. Older
          releases may spell a key differently, and the data records that spelling for each
          release. Which of these Ephervex offers as a preference is decided by the application.
          A setting marked "Removed after" no longer exists in the newest release.
        </p>
        {coverage.categories.map((category) => (
          <section key={category.name}>
            <Heading as="h3">{category.name}</Heading>
            <div className={styles.scroll}>
              <table className={styles.table}>
                <thead>
                  <tr><th>Key</th><th>Type</th><th>First release</th><th>Last release</th></tr>
                </thead>
                <tbody>
                  {category.settings.map((setting) => (
                    <tr key={setting.id}>
                      <td><code>{setting.key}</code></td>
                      <td>{setting.type}</td>
                      <td>{setting.firstRelease ? <code>{setting.firstRelease}</code> : 'Not stored'}</td>
                      <td>
                        {setting.current
                          ? 'Current'
                          : setting.lastRelease
                            ? <>Removed after <code>{setting.lastRelease}</code></>
                            : 'Not stored'}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>
        ))}
      </main>
    </Layout>
  );
}
