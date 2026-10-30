import React, {useEffect, useState} from 'react';
import styles from './SeasonalThemeNavbarItem.module.css';

type Season = 'halloween' | 'pride' | 'christmas' | 'april-fools';

const seasonNames: Record<Season, string> = {
  halloween: 'Halloween',
  pride: 'Pride',
  christmas: 'Christmas',
  'april-fools': 'April Fools',
};

declare global {
  interface Window {
    ephervexSeasonalTheme?: {
      enabled: boolean;
      season: Season | null;
      setEnabled: (enabled: boolean) => void;
    };
  }
}

export default function SeasonalThemeNavbarItem({mobile = false}: {mobile?: boolean}) {
  const [state, setState] = useState<{enabled: boolean; season: Season | null}>({enabled: true, season: null});

  useEffect(() => {
    const theme = window.ephervexSeasonalTheme;
    setState({enabled: theme?.enabled ?? true, season: theme?.season ?? null});
  }, []);

  const {enabled, season} = state;
  if (!season) return null;

  const seasonName = seasonNames[season];
  const button = (
    <button
      type="button"
      className={styles.toggle}
      aria-label={`${enabled ? 'Disable' : 'Enable'} ${seasonName} theme`}
      aria-pressed={enabled}
      onClick={() => window.ephervexSeasonalTheme?.setEnabled(!enabled)}>
      {seasonName}: {enabled ? 'On' : 'Off'}
    </button>
  );

  return mobile
    ? <li className="menu__list-item">{button}</li>
    : <span className="navbar__item">{button}</span>;
}
