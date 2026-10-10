(function () {
  const now = new Date();
  const previewHost = ['localhost', '127.0.0.1', '[::1]'].includes(location.hostname)
    || /^(?:10\.\d+\.\d+\.\d+|172\.(?:1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+)$/.test(location.hostname);
  const previewKey = 'ephervex-halloween-preview';
  const previewQuery = new URLSearchParams(location.search).get('halloweenPreview');
  let preview = false;

  if (previewHost) {
    try {
      if (previewQuery === '1' || previewQuery === '0') {
        sessionStorage.setItem(previewKey, previewQuery);
      }
      preview = sessionStorage.getItem(previewKey) === '1';
    } catch {
      preview = previewQuery === '1';
    }
  }

  if ((now.getMonth() !== 9 || now.getDate() !== 31) && !preview) return;

  document.documentElement.dataset.season = 'halloween';

  const scriptUrl = document.currentScript.src;
  const logoUrl = new URL('img/halloween-logo.png', scriptUrl).href;
  const faviconUrl = new URL('img/halloween-favicon.png', scriptUrl).href;

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
