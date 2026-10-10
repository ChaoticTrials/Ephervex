import {useState, type MouseEvent, type ReactNode} from 'react';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import useBaseUrl from '@docusaurus/useBaseUrl';
import {useColorMode} from '@docusaurus/theme-common';
import ThemeScreenshot from '@site/src/components/ThemeScreenshot';
import ScreenshotLightbox, {type LightboxSlide} from '@site/src/components/ScreenshotLightbox';
import manifest from '@site/src/components/screenshot-manifest.json';
import {catalogGroups, type CatalogEntry} from '@site/src/components/screenshot-catalog';

import styles from './screenshots.module.css';

type Variant = {file: string; width: number; height: number};
type Capture = {light: Variant[]; dark: Variant[]};
const captures = manifest.captures as Record<string, Capture>;

type Card = {state: string; region: string; title: string};

function cardsFor(entry: CatalogEntry): Card[] {
  const cards: Card[] = [];
  for (const region of ['full', ...Object.keys(entry.regions ?? {})]) {
    const key = region === 'full' ? entry.state : `${entry.state}/${region}`;
    if (!captures[key]) continue;
    cards.push({
      state: entry.state,
      region,
      title: region === 'full' ? entry.title : `${entry.title}: ${entry.regions![region]}`,
    });
  }
  return cards;
}

const cardKey = (card: Card): string =>
  card.region === 'full' ? card.state : `${card.state}/${card.region}`;

/** The largest published capture of a card in one color mode. */
function largestVariant(card: Card, mode: 'light' | 'dark'): Variant {
  const variants = captures[cardKey(card)][mode];
  return variants[variants.length - 1];
}

function ScreenshotCard({card, onOpen}: {card: Card; onOpen: () => void}): ReactNode {
  const {colorMode} = useColorMode();
  const key = cardKey(card);
  const href = useBaseUrl(`/img/ephervex/${largestVariant(card, colorMode === 'dark' ? 'dark' : 'light').file}`);
  // A plain click opens the viewer. Modified clicks keep the link's own behavior.
  const open = (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
      return;
    }
    event.preventDefault();
    onOpen();
  };
  return (
    <figure className={styles.card} id={card.region === 'full' ? card.state : `${card.state}-${card.region}`}>
      <a
        className={`${styles.frame} ${card.region === 'full' ? '' : styles.cropFrame}`}
        href={href}
        onClick={open}
        target="_blank"
        rel="noreferrer">
        <ThemeScreenshot state={card.state} region={card.region} alt={card.title} zoomable={false} />
      </a>
      <figcaption>
        <strong>{card.title}</strong>
        <span className={styles.state}>{key}</span>
      </figcaption>
    </figure>
  );
}

function Gallery(): ReactNode {
  const {colorMode} = useColorMode();
  const base = useBaseUrl('/img/ephervex/');
  const [viewing, setViewing] = useState<number | null>(null);
  const listed = new Set(catalogGroups.flatMap((group) => group.entries.map((entry) => entry.state)));
  const other = [...new Set(Object.keys(captures).map((key) => key.split('/')[0]))]
    .filter((state) => !listed.has(state))
    .sort()
    .map((state): CatalogEntry => ({state, title: state}));
  const groups = other.length
    ? [...catalogGroups, {id: 'other', title: 'Other', entries: other}]
    : catalogGroups;
  const cards = groups.flatMap((group) => group.entries.flatMap(cardsFor));
  const slides: LightboxSlide[] = cards.map((card) => {
    const variant = largestVariant(card, colorMode === 'dark' ? 'dark' : 'light');
    return {
      key: cardKey(card),
      title: card.title,
      src: `${base}${variant.file}`,
      width: variant.width,
      height: variant.height,
    };
  });
  return (
    <>
      <main className={`container ${styles.page}`}>
        <header className={styles.intro}>
          <span className={styles.eyebrow}>Interface reference</span>
          <Heading as="h1">See Ephervex in action.</Heading>
          <p>
            Every captured screen uses synthetic sample data. Images follow the site's
            color mode. Select one to open it at full size, then use the arrow buttons,
            the arrow keys, or a swipe to move between screenshots.
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
                <ScreenshotCard
                  key={cardKey(card)}
                  card={card}
                  onOpen={() => setViewing(cards.findIndex((other) => cardKey(other) === cardKey(card)))}
                />
              ))}
            </div>
          </section>
        ))}
      </main>
      {viewing !== null && (
        <ScreenshotLightbox
          slides={slides}
          index={viewing}
          onIndexChange={setViewing}
          onClose={() => setViewing(null)}
        />
      )}
    </>
  );
}

export default function Screenshots(): ReactNode {
  return (
    <Layout title="Screenshots" description="Every captured Ephervex screen, in the active color mode.">
      <Gallery />
    </Layout>
  );
}
