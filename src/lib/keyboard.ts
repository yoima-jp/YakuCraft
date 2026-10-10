type KeyboardEventLike = {
  isComposing?: boolean;
  keyCode?: number;
  nativeEvent?: Pick<KeyboardEvent, "isComposing" | "keyCode">;
};

// UI Events defines isComposing for keys inside an active composition. The legacy 229 value also
// identifies IME-handled keydowns when event ordering has already cleared isComposing.
// Keep these keys out of app shortcuts without cancelling the IME's native composition action.
// https://www.w3.org/TR/uievents/#events-keyboardevents
// https://github.com/GoogleChrome/modern-web-guidance/blob/main/skills/modern-web-guidance/guides/forms/ime-safe-enter-submit.md
export function isImeCompositionKeyDown(event: KeyboardEventLike): boolean {
  const nativeEvent = event.nativeEvent ?? event;
  return nativeEvent.isComposing === true || nativeEvent.keyCode === 229;
}
