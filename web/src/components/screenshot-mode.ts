import useIsBrowser from '@docusaurus/useIsBrowser';
import {useColorMode} from '@docusaurus/theme-common';

/** Which set of captures a page shows. */
export type ScreenshotMode = 'light' | 'dark' | 'halloween';

/**
 * The captures matching the page: the launcher's Halloween theme while the
 * site shows its Halloween season, which paints both color modes alike, and
 * otherwise the selected color mode. Static HTML always starts with light.
 */
export function useScreenshotMode(): ScreenshotMode {
  const isBrowser = useIsBrowser();
  const {colorMode} = useColorMode();
  if (!isBrowser) return 'light';
  if (document.documentElement.dataset.season === 'halloween') return 'halloween';
  return colorMode === 'dark' ? 'dark' : 'light';
}
