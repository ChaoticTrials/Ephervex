import fs from 'node:fs';
import path from 'node:path';
import type { LoadContext, Plugin } from '@docusaurus/types';
import { buildCoverage, parseOptionsDataset, type Coverage } from '../../src/options/coverage.ts';

/** The committed dataset, which is also the file the application reads as its feed. */
export const optionsDatasetPath = (siteDir: string) =>
  path.resolve(siteDir, '../crates/options/data/minecraft-options.json');

export default function minecraftOptions(context: LoadContext): Plugin<unknown> {
  return {
    name: 'ephervex-minecraft-options',
    getPathsToWatch() {
      return [optionsDatasetPath(context.siteDir)];
    },
    async loadContent() {
      const file = optionsDatasetPath(context.siteDir);
      return buildCoverage(parseOptionsDataset(JSON.parse(fs.readFileSync(file, 'utf8'))));
    },
    async contentLoaded({ content, actions }) {
      const coverage = await actions.createData(
        'minecraft-options-coverage.json',
        JSON.stringify(content as Coverage),
      );
      actions.addRoute({
        path: `${context.siteConfig.baseUrl}minecraft-options/`,
        exact: true,
        component: path.resolve(context.siteDir, 'src/options/OptionsCoverage.tsx'),
        modules: { coverage },
      });
    },
  };
}
