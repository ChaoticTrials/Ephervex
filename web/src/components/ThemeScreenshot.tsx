import React from 'react';
import useBaseUrl from '@docusaurus/useBaseUrl';
import useIsBrowser from '@docusaurus/useIsBrowser';
import {useColorMode} from '@docusaurus/theme-common';
import manifest from './screenshot-manifest.json';
import './ThemeScreenshot.css';

type Variant = {
  window: string;
  width: number;
  height: number;
  file: string;
};

type Capture = {
  light: Variant[];
  dark: Variant[];
};

const captures = manifest.captures as Record<string, Capture>;

export type ThemeScreenshotProps = {
  state: string;
  region?: string;
  alt: string;
  className?: string;
};

/** Displays only the capture for the site's selected color mode. */
export default function ThemeScreenshot({
  state,
  region = 'full',
  alt,
  className,
}: ThemeScreenshotProps): React.JSX.Element {
  const base = useBaseUrl('/img/ephervex/');
  const isBrowser = useIsBrowser();
  const {colorMode} = useColorMode();
  const capture = captures[`${state}/${region}`];
  if (!capture) {
    throw new Error(`Missing screenshot state ${state}/${region}. Run npm run screenshots:sync.`);
  }
  const variants = capture.light;
  const fallback = variants[0];
  if (!fallback) {
    throw new Error(`Missing screenshot sources for ${state}/${region}. Run npm run screenshots:sync.`);
  }

  // Docusaurus renders this placeholder on the server and during hydration.
  // Adding only the active image afterward avoids loading the hidden theme.
  const activeTheme = isBrowser && colorMode === document.documentElement.dataset.theme
    ? colorMode as 'light' | 'dark'
    : null;
  const activeVariants = activeTheme ? capture[activeTheme] : [];
  const activeFallback = activeVariants[0];

  return (
    <span
      className={`themeScreenshot ${className ?? ''}`}
      style={{aspectRatio: `${fallback.width} / ${fallback.height}`, maxWidth: fallback.width}}>
      {activeFallback && (
        <picture>
          {activeVariants.slice(1).reverse().map((variant) => {
            const windowWidth = Number(variant.window.split('x')[0]);
            return (
              <source
                key={variant.window}
                type="image/webp"
                media={`(min-width: ${Math.ceil(windowWidth / 2)}px)`}
                srcSet={`${base}${variant.file}`}
              />
            );
          })}
          <img
            src={`${base}${activeFallback.file}`}
            width={activeFallback.width}
            height={activeFallback.height}
            alt={alt}
            loading="lazy"
            decoding="async"
          />
        </picture>
      )}
    </span>
  );
}
