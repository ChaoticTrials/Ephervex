import React, {useEffect, useState} from 'react';
import styles from './SeasonalThemeNavbarItem.module.css';

declare global {
  interface Window {
    ephervexSeasonalTheme?: {
      enabled: boolean;
      setEnabled: (enabled: boolean) => void;
    };
  }
}

export default function SeasonalThemeNavbarItem({mobile = false}: {mobile?: boolean}) {
  const [enabled, setEnabled] = useState(true);

  useEffect(() => {
    setEnabled(window.ephervexSeasonalTheme?.enabled ?? true);
  }, []);

  const button = (
    <button
      type="button"
      className={styles.toggle}
      aria-label={`${enabled ? 'Disable' : 'Enable'} seasonal themes`}
      aria-pressed={enabled}
      onClick={() => window.ephervexSeasonalTheme?.setEnabled(!enabled)}>
      Seasonal: {enabled ? 'On' : 'Off'}
    </button>
  );

  return mobile
    ? <li className="menu__list-item">{button}</li>
    : <span className="navbar__item">{button}</span>;
}
