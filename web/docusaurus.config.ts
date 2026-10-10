import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const repository = 'https://github.com/ChaoticTrials/Ephervex';

const config: Config = {
  title: 'Ephervex',
  tagline: 'Disposable Minecraft environments for reproducing bugs.',
  favicon: 'img/favicon.png',
  future: {v4: true},
  url: 'https://ephervex.chaotictrials.de',
  baseUrl: process.env.DOCUSAURUS_BASE_URL ?? '/',
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
        theme: {customCss: './src/css/custom.css'},
      } satisfies Preset.Options,
    ],
  ],
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
        {type: 'docsVersionDropdown', position: 'right'},
        {href: repository, label: 'GitHub', position: 'right'},
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Documentation',
          items: [
            {label: 'Introduction', to: '/docs/v1/intro'},
            {label: 'Feature List', to: '/docs/v1/feature-list'},
            {label: 'Installation', to: '/install'},
            {label: 'Interface tour', to: '/docs/v1/interface-tour'},
          ],
        },
        {
          title: 'Project',
          items: [
            {label: 'Source code', href: repository},
            {label: 'Downloads', href: `${repository}/releases`},
            {label: 'Report an issue', href: `${repository}/issues`},
          ],
        },
      ],
      copyright: `Copyright &copy; ${new Date().getFullYear()} Ephervex contributors. Ephervex is not affiliated with or endorsed by Mojang or Microsoft.`,
    },
    prism: {theme: prismThemes.github, darkTheme: prismThemes.dracula},
  } satisfies Preset.ThemeConfig,
};

export default config;
