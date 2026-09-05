import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";
import { REPO_URL, SITE_BASE, SITE_DESCRIPTION, SITE_TITLE, SITE_URL } from "./src/lib/site-meta.mjs";
// Written by scripts/sync-content.mjs before every dev/build — the sidebar is
// derived from docs/src/SUMMARY.md, never typed here. `npm run sync` if this
// import is missing. (The mdBook-era `.html` redirects are static stubs the
// same script writes under public/, not Astro `redirects`: see the script.)
import { sidebar } from "./src/sidebar.generated.mjs";

// gemini-rs's documentation website. Content is NOT authored here — the one
// curated page (the landing) lives in src/content/docs, and every other page
// is rendered from the repo's canonical docs/ markdown by
// scripts/sync-content.mjs before every dev/build (see package.json). docs/
// stays the single source of truth and docs/src/SUMMARY.md stays the single
// source of *structure*; this app is presentation only.
//
// The rustdoc API reference is not part of this build: the docs workflow runs
// `cargo doc --all-features` and copies target/doc into dist/api after
// `astro build`, so /api/… links work on the deployed site and nowhere else.
//
// The theme is the "Modernist Functionalism" (Braun/Dieter Rams) system shared
// with vamsiramakrishnan/anvil — src/styles/custom.css is that brand layer
// (accent #00408b, Hanken Grotesk + JetBrains Mono, hairline borders,
// github-dark code), trimmed to the components this site uses.
export default defineConfig({
  site: SITE_URL,
  base: SITE_BASE,
  integrations: [
    starlight({
      title: SITE_TITLE,
      description: SITE_DESCRIPTION,
      favicon: "/favicon.svg",
      customCss: ["./src/styles/custom.css"],
      components: {
        // The header wordmark: an accent monogram plate + "gemini-rs" logotype.
        SiteTitle: "./src/components/SiteTitle.astro",
      },
      social: [
        { icon: "github", label: "GitHub", href: REPO_URL },
        { icon: "seti:rust", label: "crates.io", href: "https://crates.io/crates/gemini-adk-fluent-rs" },
      ],
      // Synced pages carry a per-page `editUrl` pointing back at their docs/
      // source; this is the fallback for the curated landing.
      editLink: { baseUrl: `${REPO_URL}/edit/main/apps/docs/` },
      expressiveCode: {
        // One dark theme in BOTH site themes: code is terminal content, so it
        // always renders behind the instrument's black readout glass.
        themes: ["github-dark"],
        styleOverrides: { borderRadius: "0.5rem" },
      },
      sidebar,
    }),
  ],
});
