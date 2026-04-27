import './styles.css';
import './components/tcode-app';

function updateViewportHeight(): void {
  const height = window.visualViewport?.height ?? window.innerHeight;
  document.documentElement.style.setProperty('--tcode-viewport-height', `${height}px`);
}

let viewportUpdateFrame: number | null = null;

function scheduleViewportHeightUpdate(): void {
  if (viewportUpdateFrame !== null) {
    return;
  }

  viewportUpdateFrame = window.requestAnimationFrame(() => {
    viewportUpdateFrame = null;
    updateViewportHeight();
  });
}

function removeViewportHeightListeners(): void {
  window.removeEventListener('resize', scheduleViewportHeightUpdate);
  window.removeEventListener('orientationchange', scheduleViewportHeightUpdate);
  window.visualViewport?.removeEventListener('resize', scheduleViewportHeightUpdate);
  window.visualViewport?.removeEventListener('scroll', scheduleViewportHeightUpdate);
  if (viewportUpdateFrame !== null) {
    window.cancelAnimationFrame(viewportUpdateFrame);
    viewportUpdateFrame = null;
  }
}

updateViewportHeight();
window.addEventListener('resize', scheduleViewportHeightUpdate, { passive: true });
window.addEventListener('orientationchange', scheduleViewportHeightUpdate, { passive: true });
window.visualViewport?.addEventListener('resize', scheduleViewportHeightUpdate, { passive: true });
window.visualViewport?.addEventListener('scroll', scheduleViewportHeightUpdate, { passive: true });

import.meta.hot?.dispose(removeViewportHeightListeners);
