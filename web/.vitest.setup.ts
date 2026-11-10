import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// vitest runs with `globals: false`, so Testing Library's automatic cleanup
// does not register itself. Without this, every `render` in a file leaks into
// the next test and role/text queries start matching across tests.
afterEach(() => {
  cleanup();
});

// Radix primitives used by the console (Select, Tabs, Tooltip) rely on a few
// DOM APIs jsdom does not implement.
if (!Element.prototype.hasOwnProperty("hasPointerCapture")) {
  Element.prototype.hasPointerCapture = () => false;
}
if (!Element.prototype.hasOwnProperty("setPointerCapture")) {
  Element.prototype.setPointerCapture = () => {};
}
if (!Element.prototype.hasOwnProperty("releasePointerCapture")) {
  Element.prototype.releasePointerCapture = () => {};
}
if (!Element.prototype.hasOwnProperty("scrollIntoView")) {
  Element.prototype.scrollIntoView = () => {};
}
if (!window.matchMedia) {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}
