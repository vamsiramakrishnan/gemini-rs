# apps/docs — the documentation website

Astro + Starlight. **Content is not authored here.** Every page except the
landing is rendered from the repo's canonical markdown under `docs/` by
`scripts/sync-content.mjs` before every dev/build, and `docs/src/SUMMARY.md`
is the single source of *structure*: its `# Section` headings become sidebar
groups, its entries become pages, in that order. This app is presentation
only — theme, header, landing page.

```bash
cd apps/docs
npm install
npm run dev        # sync docs/ → src/content/docs, then astro dev on :4321
npm run build      # sync + check every internal link + astro build → dist/
```

Or from the repo root: `just docs-site` / `just docs-site-build`.

## What lives where

| Path | Role |
|------|------|
| `docs/src/SUMMARY.md` | Sidebar groups, page order, page labels — edit this to move a page |
| `docs/src/*.md`, `docs/user-guide/*.md` | The pages. Edit these. `editUrl` on every rendered page points here |
| `docs/assets/` | Diagrams and screenshots; copied to `public/assets/` at sync time |
| `src/content/docs/index.mdx` | The one curated page — the landing. Checked in |
| `src/styles/custom.css` | The brand layer: the Modernist Functionalism system shared with `anvil`, trimmed to what this site uses |
| `src/components/SiteTitle.astro` | The header wordmark |
| `scripts/sync-content.mjs` | SUMMARY.md → pages, sidebar, mdBook-era redirects, assets. Everything it writes is gitignored |
| `scripts/check-content.mjs` | Fails the build if any synced page lacks a title or links to a route the build will not emit |

## How links work

Pages in `docs/` use repository-relative links so they read correctly on
GitHub. The sync rewrites each one for the site: a link to another docs page
becomes its route (`/gemini-rs/compose/flow/`), `./api/…` becomes the rustdoc
tree, `./assets/…` becomes `public/assets`, and a link to any other repository
file becomes a GitHub URL. External and fragment links are untouched.

## The rustdoc API reference

It is not part of this build. The docs workflow runs
`cargo doc --workspace --all-features` and copies `target/doc` into `dist/api`
after `astro build`, so `/gemini-rs/api/…` works on the deployed site and
nowhere else — `check-content` skips those links for that reason, and the
README's *API reference* link keeps its old URL.

## Old URLs

mdBook served `docs/src/foo.md` at `/foo.html` and `user-guide/foo.md` at
`/user-guide/foo.html`. The sync emits a redirect for every one of those to
the page's new route, so nothing that ever linked to the book breaks.
