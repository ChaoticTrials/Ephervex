import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';
import fs from 'node:fs';
import publication from './archive/publication.json';
import roadToV1 from './plugins/road-to-v1/index';

const repository = 'https://github.com/ChaoticTrials/Ephervex';
const baseUrl = process.env.DOCUSAURUS_BASE_URL ?? '/';
const archivedPull3 = (JSON.parse(fs.readFileSync(new URL('./archive/archive-data.json', import.meta.url), 'utf8')) as {
  pulls: { number: number }[];
}).pulls.some((entry) => entry.number === 3);

const config: Config = {
  title: 'Ephervex',
  tagline: 'Disposable Minecraft environments for reproducing bugs.',
  favicon: 'img/favicon.png',
  scripts: [{src: `${baseUrl}halloween-theme.js`}],
  future: {v4: true},
  url: 'https://ephervex.chaotictrials.de',
  baseUrl,
  organizationName: 'ChaoticTrials',
  projectName: 'Ephervex',
  onBrokenLinks: 'throw',
  i18n: {defaultLocale: 'en', locales: ['en']},
  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          editUrl: `${repository}/edit/main/web/`,
          lastVersion: 'current',
          versions: {
            current: {label: 'v1', path: 'v1', banner: 'none'},
          },
        },
        blog: false,
        sitemap: {
          ignorePatterns: publication.mode === 'testing'
            ? ['/road-to-v1/**']
            : archivedPull3 ? [] : ['/road-to-v1/forgejo/pulls/3/'],
        },
        theme: {customCss: './src/css/custom.css'},
      } satisfies Preset.Options,
    ],
  ],
  plugins: [roadToV1],
  themeConfig: {
    image: 'img/ephervex-social.png',
    colorMode: {respectPrefersColorScheme: true},
    navbar: {
      title: 'Ephervex',
      logo: {alt: 'Ephervex logo', src: 'img/ephervex-logo.png'},
      items: [
        {type: 'docSidebar', sidebarId: 'docsSidebar', position: 'left', label: 'Documentation'},
        {to: '/install', label: 'Install', position: 'left'},
        {to: '/screenshots', label: 'Screenshots', position: 'left'},
        {to: '/road-to-v1/', label: 'Road to v1', position: 'left'},
        {type: 'docsVersionDropdown', position: 'right'},
        {href: repository, label: 'GitHub', position: 'right'},
      ],
    },
    prism: {theme: prismThemes.dracula, darkTheme: prismThemes.dracula},
  } satisfies Preset.ThemeConfig,
};

export default config;
