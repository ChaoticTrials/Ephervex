(function () {
  const now = new Date();
  const previewHost = ['localhost', '127.0.0.1', '[::1]'].includes(location.hostname)
    || /^(?:10\.\d+\.\d+\.\d+|172\.(?:1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+)$/.test(location.hostname);
  const previewKey = 'ephervex-season-preview';
  const preferenceKey = 'ephervex-seasonal-theme';
  const query = new URLSearchParams(location.search);
  const previewQuery = query.get('seasonPreview');
  const seasons = ['halloween', 'pride', 'christmas', 'april-fools'];
  let preview = null;
  let enabled = true;

  try {
    enabled = localStorage.getItem(preferenceKey) !== 'off';
  } catch {
    // Private browsing settings can make local storage unavailable.
  }

  if (previewHost) {
    try {
      if (seasons.includes(previewQuery) || previewQuery === 'off') {
        sessionStorage.setItem(previewKey, previewQuery);
      }
      preview = sessionStorage.getItem(previewKey);
    } catch {
      preview = previewQuery;
    }
  }

  const scheduled = now.getMonth() === 3 && now.getDate() === 1 ? 'april-fools'
    : now.getMonth() === 11 && now.getDate() >= 24 && now.getDate() <= 26 ? 'christmas'
    : now.getMonth() === 5 ? 'pride'
    : now.getMonth() === 9 && now.getDate() === 31 ? 'halloween' : null;
  const availableSeason = preview === 'off' ? null : seasons.includes(preview) ? preview : scheduled;
  globalThis.ephervexSeasonalTheme = {
    enabled,
    season: availableSeason,
    setEnabled(value) {
      try {
        localStorage.setItem(preferenceKey, value ? 'on' : 'off');
        location.reload();
      } catch {
        // Keep the current page usable when storage is unavailable.
      }
    },
  };

  const season = enabled ? availableSeason : null;
  if (!season) return;

  document.documentElement.dataset.season = season;

  const scriptUrl = document.currentScript.src;
  const logoUrl = new URL(`img/${season}-logo.png`, scriptUrl).href;
  const faviconUrl = new URL(`img/${season}-favicon.png`, scriptUrl).href;

  function updateBranding() {
    const favicon = document.querySelector('link[rel="icon"]');
    if (favicon && favicon.href !== faviconUrl) favicon.href = faviconUrl;

    document.querySelectorAll('.navbar__logo img, footer img').forEach((image) => {
      if (image.src !== logoUrl) image.src = logoUrl;
    });
  }

  document.addEventListener('DOMContentLoaded', () => {
    updateBranding();
    const observer = new MutationObserver(updateBranding);
    observer.observe(document.head, {childList: true, subtree: true, attributes: true, attributeFilter: ['href']});
    observer.observe(document.body, {childList: true, subtree: true, attributes: true, attributeFilter: ['src']});
  }, {once: true});
})();
