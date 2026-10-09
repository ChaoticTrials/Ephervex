import type {ReactNode} from 'react';
import Link from '@docusaurus/Link';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import ThemeScreenshot from '@site/src/components/ThemeScreenshot';
import styles from './index.module.css';

const features = [
  {
    label: 'Exact versions',
    title: 'Use the versions you select.',
    description: 'Choose the Minecraft release, loader, and loader version. Ephervex keeps your selection exact.',
  },
  {
    label: 'Temporary instances',
    title: 'Discard or keep an instance.',
    description: 'Use a temporary instance for a quick reproduction, or make it persistent when you need to return to it.',
  },
  {
    label: 'Logs and diagnostics',
    title: 'Inspect a failure.',
    description: 'Inspect logs, crash reports, diagnostics, and snapshots from the desktop or command line.',
  },
];

export default function Home(): ReactNode {
  return (
    <Layout description="A Linux Minecraft launcher for disposable bug-reproduction environments, with a desktop interface and CLI.">
      <main>
        <section className={styles.hero}>
          <div className={`container ${styles.heroInner}`}>
            <div className={styles.heroCopy}>
              <span className={styles.eyebrow}>Linux desktop + CLI</span>
              <Heading as="h1">Reproduce Minecraft bugs.<br /><span>Use a fresh environment.</span></Heading>
              <p>
                Create a fresh Minecraft environment with exact game and loader versions.
                Investigate the problem, then discard the instance or keep it for later.
              </p>
              <div className={styles.heroActions}>
                <Link className={styles.primaryAction} to="/docs/v1/install">Install Ephervex <span aria-hidden="true">↗</span></Link>
                <Link className={styles.secondaryAction} to="/docs/v1/intro">Read the docs <span aria-hidden="true">→</span></Link>
              </div>
              <div className={styles.supportLine}>
                <span>Vanilla</span><span>Forge</span><span>NeoForge</span>
                <span>Minecraft 1.12.2 and newer</span>
              </div>
            </div>
            <div className={styles.heroVisual}>
              <div className={styles.windowBar}>
                <span className={styles.windowDots} aria-hidden="true"><i /><i /><i /></span>
                <span>Ephervex / Instances</span>
              </div>
              <ThemeScreenshot state="instances-overview" alt="Ephervex instance list with a selected Minecraft environment" />
            </div>
          </div>
        </section>

        <section className={styles.features} aria-labelledby="features-title">
          <div className="container">
            <div className={styles.sectionIntro}>
              <span className={styles.sectionLabel}>What Ephervex does</span>
              <Heading as="h2" id="features-title">Create an environment and inspect a failure.</Heading>
              <p>Set up the conditions you need without filling your everyday launcher with test instances.</p>
            </div>
            <div className={styles.featureGrid}>
              {features.map((feature) => (
                <article className={styles.featureCard} key={feature.label}>
                  <span className={styles.cardAccent} aria-hidden="true" />
                  <span className={styles.cardLabel}>{feature.label}</span>
                  <Heading as="h3">{feature.title}</Heading>
                  <p>{feature.description}</p>
                </article>
              ))}
            </div>
          </div>
        </section>

        <section className={styles.showcase} aria-labelledby="showcase-title">
          <div className="container">
            <div className={styles.sectionIntro}>
              <span className={styles.sectionLabel}>Desktop interface</span>
              <Heading as="h2" id="showcase-title">Work with instances, mods, and servers.</Heading>
              <p>These captures use synthetic sample data and follow the site's light or dark mode.</p>
            </div>
            <div className={styles.showcaseGrid}>
              <article className={styles.showcaseCard}>
                <div className={styles.showcaseImage}>
                  <ThemeScreenshot state="client-mods" alt="Mod manager and compatible mod catalog in Ephervex" />
                </div>
                <div className={styles.showcaseText}>
                  <div><span className={styles.cardLabel}>Manage mods</span><Heading as="h3">Check mod compatibility.</Heading></div>
                  <p>Browse compatible mods, review dependencies, and isolate a failure in a large mod set.</p>
                </div>
              </article>
              <article className={styles.showcaseCard}>
                <div className={styles.showcaseImage}>
                  <ThemeScreenshot state="server-console" alt="Ephervex server console with live output" />
                </div>
                <div className={styles.showcaseText}>
                  <div><span className={styles.cardLabel}>Linked servers</span><Heading as="h3">Run a client and server.</Heading></div>
                  <p>Run a server beside a client and inspect its console when the problem crosses the connection.</p>
                </div>
              </article>
            </div>
            <div className={styles.showcaseLinks}>
              <Link to="/docs/v1/interface-tour">Take the interface tour <span aria-hidden="true">→</span></Link>
              <Link to="/screenshots">Browse all screenshots <span aria-hidden="true">→</span></Link>
            </div>
          </div>
        </section>

        <section className={styles.cta}>
          <div className={`container ${styles.ctaInner}`}>
            <div>
              <span className={styles.sectionLabel}>Get started</span>
              <Heading as="h2">Install Ephervex for Linux.</Heading>
            </div>
            <Link className={styles.primaryAction} to="/docs/v1/install">Installation guide <span aria-hidden="true">→</span></Link>
          </div>
        </section>
      </main>
    </Layout>
  );
}
