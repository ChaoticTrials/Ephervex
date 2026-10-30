import type {ReactNode} from 'react';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import styles from './styles.module.css';

const repository = 'https://github.com/ChaoticTrials/Ephervex';

const groups = [
  {
    title: 'Get started',
    links: [
      {label: 'Install Ephervex', to: '/install'},
      {label: 'Getting Started', to: '/docs/v1/getting-started'},
      {label: 'Feature List', to: '/docs/v1/feature-list'},
    ],
  },
  {
    title: 'Explore',
    links: [
      {label: 'Interface tour', to: '/docs/v1/interface-tour'},
      {label: 'Command Line', to: '/docs/v1/command-line'},
      {label: 'Screenshots', to: '/screenshots'},
    ],
  },
  {
    title: 'Project',
    links: [
      {label: 'Source code', to: repository},
      {label: 'Releases', to: `${repository}/releases`},
      {label: 'Report an issue', to: `${repository}/issues`},
    ],
  },
];

export default function Footer(): ReactNode {
  const logoUrl = useBaseUrl('/img/ephervex-logo.png');

  return (
    <footer className={styles.footer}>
      <div className={`container ${styles.inner}`}>
        <div className={styles.main}>
          <div className={styles.identity}>
            <Link className={styles.brand} to="/" aria-label="Ephervex home">
              <img src={logoUrl} alt="" width="40" height="40" />
              <span>Ephervex</span>
            </Link>
            <p>Disposable Minecraft environments for reproducing bugs. Create an instance with exact versions, inspect the result, and keep or remove it.</p>
          </div>

          <div className={styles.groups}>
            {groups.map((group) => (
              <nav aria-label={group.title} className={styles.group} key={group.title}>
                <h2>{group.title}</h2>
                <ul>
                  {group.links.map((link) => (
                    <li key={link.label}><Link to={link.to}>{link.label}</Link></li>
                  ))}
                </ul>
              </nav>
            ))}
          </div>
        </div>

        <div className={styles.bottom}>
          <span>&copy; {new Date().getFullYear()} Ephervex contributors</span>
          <span>Ephervex is not affiliated with or endorsed by Mojang or Microsoft.</span>
        </div>
      </div>
    </footer>
  );
}
