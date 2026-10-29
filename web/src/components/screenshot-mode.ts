import useIsBrowser from '@docusaurus/useIsBrowser';
import {useColorMode} from '@docusaurus/theme-common';

/** Which set of captures a page shows. */
export const seasonalModes = ['halloween', 'christmas', 'pride', 'april-fools'] as const;
export type ScreenshotMode = 'light' | 'dark' | (typeof seasonalModes)[number];

/**
 * The captures matching the page: the launcher's seasonal theme while the
 * site shows that season, which paints both color modes alike, and otherwise
 * the selected color mode. Static HTML always starts with light.
 */
export function useScreenshotMode(): ScreenshotMode {
  const isBrowser = useIsBrowser();
  const {colorMode} = useColorMode();
  if (!isBrowser) return 'light';
  const season = seasonalModes.find((mode) => mode === document.documentElement.dataset.season);
  if (season) return season;
  return colorMode === 'dark' ? 'dark' : 'light';
}
