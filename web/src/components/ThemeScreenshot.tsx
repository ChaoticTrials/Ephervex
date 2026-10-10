import React from 'react';
import useBaseUrl from '@docusaurus/useBaseUrl';
import useIsBrowser from '@docusaurus/useIsBrowser';
import {useColorMode} from '@docusaurus/theme-common';
import manifest from './screenshot-manifest.json';
import ScreenshotLightbox from './ScreenshotLightbox';
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
  zoomable?: boolean;
};

/** Displays only the capture for the site's selected color mode. */
export default function ThemeScreenshot({
  state,
  region = 'full',
  alt,
  className,
  zoomable = true,
}: ThemeScreenshotProps): React.JSX.Element {
  const [viewing, setViewing] = React.useState(false);
  const base = useBaseUrl('/img/ephervex/');
  const isBrowser = useIsBrowser();
  const {colorMode} = useColorMode();
  const key = region === 'full' ? state : `${state}/${region}`;
  const capture = captures[key];
  if (!capture) {
    throw new Error(`Missing screenshot state ${key}. Run npm run screenshots:sync.`);
  }
  const variants = capture.light;
  const fallback = variants[0];
  if (!fallback) {
    throw new Error(`Missing screenshot sources for ${key}. Run npm run screenshots:sync.`);
  }

  // Render a light capture in static HTML, then follow the selected color mode.
  const activeTheme = isBrowser && colorMode === 'dark' ? 'dark' : 'light';
  const activeVariants = capture[activeTheme];
  const activeFallback = activeVariants[0];
  const fullSize = activeVariants[activeVariants.length - 1];
  const frameStyle = {aspectRatio: `${fallback.width} / ${fallback.height}`, maxWidth: fallback.width};
  const picture = activeFallback && (
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
  );

  return (
    <>
      {zoomable ? (
        <button
          type="button"
          className={`themeScreenshot themeScreenshotZoomable ${className ?? ''}`}
          style={frameStyle}
          aria-label={`Enlarge screenshot: ${alt}`}
          disabled={!activeFallback}
          onClick={() => setViewing(true)}>
          {picture}
          <span className="themeScreenshotZoomHint" aria-hidden="true">View larger</span>
        </button>
      ) : (
        <span className={`themeScreenshot ${className ?? ''}`} style={frameStyle}>
          {picture}
        </span>
      )}
      {viewing && fullSize && (
        <ScreenshotLightbox
          slides={[{
            key,
            title: alt,
            src: `${base}${fullSize.file}`,
            width: fullSize.width,
            height: fullSize.height,
          }]}
          index={0}
          onClose={() => setViewing(false)}
        />
      )}
    </>
  );
}
