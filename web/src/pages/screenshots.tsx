import type {ReactNode} from 'react';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import useBaseUrl from '@docusaurus/useBaseUrl';
import {useColorMode} from '@docusaurus/theme-common';
import ThemeScreenshot from '@site/src/components/ThemeScreenshot';
import manifest from '@site/src/components/screenshot-manifest.json';
import {catalogGroups, type CatalogEntry} from '@site/src/components/screenshot-catalog';

import styles from './screenshots.module.css';

type Capture = {light: {file: string}[]; dark: {file: string}[]};
const captures = manifest.captures as Record<string, Capture>;

type Card = {state: string; region: string; title: string};

function cardsFor(entry: CatalogEntry): Card[] {
  const cards: Card[] = [];
  for (const region of ['full', ...Object.keys(entry.regions ?? {})]) {
    if (!captures[`${entry.state}/${region}`]) continue;
    cards.push({
      state: entry.state,
      region,
      title: region === 'full' ? entry.title : `${entry.title}: ${entry.regions![region]}`,
    });
  }
  return cards;
}

function ScreenshotCard({card}: {card: Card}): ReactNode {
  const {colorMode} = useColorMode();
  const capture = captures[`${card.state}/${card.region}`];
  const variants = capture[colorMode === 'dark' ? 'dark' : 'light'];
  const href = useBaseUrl(`/img/ephervex/${variants[variants.length - 1].file}`);
  return (
    <figure className={styles.card} id={`${card.state}-${card.region}`}>
      <a
        className={`${styles.frame} ${card.region === 'full' ? '' : styles.cropFrame}`}
        href={href}
        target="_blank"
        rel="noreferrer">
        <ThemeScreenshot state={card.state} region={card.region} alt={card.title} />
      </a>
      <figcaption>
        <strong>{card.title}</strong>
        <span className={styles.state}>{card.state}/{card.region}</span>
      </figcaption>
    </figure>
  );
}

export default function Screenshots(): ReactNode {
  const listed = new Set(catalogGroups.flatMap((group) => group.entries.map((entry) => entry.state)));
  const other = [...new Set(Object.keys(captures).map((key) => key.split('/')[0]))]
    .filter((state) => !listed.has(state))
    .sort()
    .map((state): CatalogEntry => ({state, title: state}));
  const groups = other.length
    ? [...catalogGroups, {id: 'other', title: 'Other', entries: other}]
    : catalogGroups;
  return (
    <Layout title="Screenshots" description="Every captured Ephervex screen, in the active color mode.">
      <main className={`container ${styles.page}`}>
        <header className={styles.intro}>
          <span className={styles.eyebrow}>Interface reference</span>
          <Heading as="h1">See Ephervex in action.</Heading>
          <p>
            Every captured screen uses synthetic sample data. Images follow the site's
            color mode. Select one to open it at full size.
          </p>
        </header>
        <nav className={styles.toc} aria-label="Screenshot categories">
          {groups.map((group) => (
            <a key={group.id} href={`#${group.id}`}>{group.title}</a>
          ))}
        </nav>
        {groups.map((group) => (
          <section key={group.id} id={group.id} className={styles.group}>
            <Heading as="h2">{group.title}</Heading>
            <div className={styles.grid}>
              {group.entries.flatMap(cardsFor).map((card) => (
                <ScreenshotCard key={`${card.state}/${card.region}`} card={card} />
              ))}
            </div>
          </section>
        ))}
      </main>
    </Layout>
  );
}
