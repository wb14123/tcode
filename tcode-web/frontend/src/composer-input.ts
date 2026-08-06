/**
 * Keyboard-decision helpers for the composer textarea.
 *
 * Kept free of DOM imports so the logic can be unit-tested under plain node
 * (see test.mjs) without a browser or jsdom.
 */

/** The subset of a KeyboardEvent the submit decision depends on. */
export interface EnterKeyState {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  isComposing: boolean;
}

/**
 * Decide whether a keydown event should be intercepted as "submit the message".
 *
 * On coarse-pointer devices plain Enter always inserts a newline and
 * submission happens via the send button, so no keydown is ever intercepted.
 * On fine-pointer devices, plain Enter (no modifiers, not IME composing)
 * submits.
 */
export function shouldSubmitOnEnter(coarsePointer: boolean, event: EnterKeyState): boolean {
  if (coarsePointer) {
    return false;
  }
  return (
    event.key === 'Enter' &&
    !event.shiftKey &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !event.isComposing
  );
}
