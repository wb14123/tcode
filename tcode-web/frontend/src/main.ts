import './styles.css';
import './components/tcode-app';

const KEYBOARD_INSET_PROPERTY = '--tcode-keyboard-inset';
let keyboardInsetFrame: number | null = null;

function isKeyboardInput(element: Element | null): boolean {
  if (!(element instanceof HTMLElement)) {
    return false;
  }

  if (element.isContentEditable) {
    return true;
  }

  if (element instanceof HTMLTextAreaElement || element instanceof HTMLSelectElement) {
    return true;
  }

  if (!(element instanceof HTMLInputElement)) {
    return false;
  }

  return !['button', 'checkbox', 'color', 'file', 'hidden', 'image', 'radio', 'range', 'reset', 'submit'].includes(
    element.type,
  );
}

function getKeyboardInset(): number {
  const visualViewport = window.visualViewport;
  if (!visualViewport || !isKeyboardInput(document.activeElement)) {
    return 0;
  }

  return Math.max(0, Math.round(window.innerHeight - visualViewport.height - visualViewport.offsetTop));
}

function updateKeyboardInset(): void {
  document.documentElement.style.setProperty(KEYBOARD_INSET_PROPERTY, `${getKeyboardInset()}px`);
}

function scheduleKeyboardInsetUpdate(): void {
  if (keyboardInsetFrame !== null) {
    return;
  }

  keyboardInsetFrame = window.requestAnimationFrame(() => {
    keyboardInsetFrame = null;
    updateKeyboardInset();
  });
}

function removeKeyboardInsetListeners(): void {
  window.removeEventListener('resize', scheduleKeyboardInsetUpdate);
  window.removeEventListener('orientationchange', scheduleKeyboardInsetUpdate);
  window.removeEventListener('focusin', scheduleKeyboardInsetUpdate);
  window.removeEventListener('focusout', scheduleKeyboardInsetUpdate);
  window.visualViewport?.removeEventListener('resize', scheduleKeyboardInsetUpdate);
  window.visualViewport?.removeEventListener('scroll', scheduleKeyboardInsetUpdate);

  if (keyboardInsetFrame !== null) {
    window.cancelAnimationFrame(keyboardInsetFrame);
    keyboardInsetFrame = null;
  }
}

updateKeyboardInset();
window.addEventListener('resize', scheduleKeyboardInsetUpdate, { passive: true });
window.addEventListener('orientationchange', scheduleKeyboardInsetUpdate, { passive: true });
window.addEventListener('focusin', scheduleKeyboardInsetUpdate, { passive: true });
window.addEventListener('focusout', scheduleKeyboardInsetUpdate, { passive: true });
window.visualViewport?.addEventListener('resize', scheduleKeyboardInsetUpdate, { passive: true });
window.visualViewport?.addEventListener('scroll', scheduleKeyboardInsetUpdate, { passive: true });

import.meta.hot?.dispose(removeKeyboardInsetListeners);
