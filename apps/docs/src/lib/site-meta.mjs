// The site's identity, shared by astro.config.mjs (the Starlight header) and the
// content sync script (every absolute link it writes) — one source, no
// hand-mirrored copies.
export const SITE_TITLE = "gemini-rs";
export const SITE_DESCRIPTION =
  "Full Rust SDK for the Gemini Multimodal Live API — one wire protocol, one governed runtime, one fluent API, in three layered crates.";

// GitHub Pages coordinates. `site` + `base` produce the published URL
// https://vamsiramakrishnan.github.io/gemini-rs/ and every absolute link the
// theme builds. Change `base` if the repo (and thus the Pages path) is renamed.
export const SITE_URL = "https://vamsiramakrishnan.github.io";
export const SITE_BASE = "/gemini-rs";
export const REPO_URL = "https://github.com/vamsiramakrishnan/gemini-rs";
