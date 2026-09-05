#!/usr/bin/env node
// Build-time content sync: the repo's canonical markdown (docs/) → this site's
// Starlight content tree. The website is a *view* of those files, never a
// second copy to maintain — this runs before every `astro dev`/`astro build`
// (see package.json), and everything it writes is gitignored. Edit the source
// markdown, not the generated pages.
//
// docs/src/SUMMARY.md is the single source of *structure*: its `# Section`
// headings become sidebar groups, its list entries become pages, in that
// order. Nothing about the sidebar is typed into astro.config.mjs.
//
// The transform per page is deliberately small:
//   1. drop the leading H1 (Starlight renders the title from frontmatter)
//   2. synthesize frontmatter: title, description, sidebar label + order,
//      and an editUrl pointing at the real source file on GitHub
//   3. rewrite relative links — to another page: its site route; to the
//      rustdoc tree: /api/…; to a diagram: /assets/…; to any other repository
//      file: GitHub
//   4. emit `.md` (NOT `.mdx`) so raw prose tokens (`<`, `{`) never need escaping
//
// It also writes src/sidebar.generated.mjs (the sidebar, plus redirects from
// every mdBook-era `.html` route so inbound links keep working) and copies
// docs/assets into public/assets.
//
//   node apps/docs/scripts/sync-content.mjs [--dry-run]
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, posix } from "node:path";
import { fileURLToPath } from "node:url";
import { REPO_URL, SITE_BASE, SITE_URL } from "../src/lib/site-meta.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const APP = join(HERE, "..");
const REPO_ROOT = join(APP, "..", "..");
const DOCS_SRC = join(REPO_ROOT, "docs", "src");
const CONTENT = join(APP, "src", "content", "docs");
const PUBLIC = join(APP, "public");
const PUBLIC_ASSETS = join(PUBLIC, "assets");
const SIDEBAR_OUT = join(APP, "src", "sidebar.generated.mjs");
const ASTRO_CONTENT_CACHE = join(APP, "node_modules", ".astro", "data-store.json");
const DRY_RUN = process.argv.includes("--dry-run");

/** SUMMARY.md section heading → URL directory. Unknown headings are slugified. */
const SECTION_DIRS = {
  "Getting Started": "start",
  "Voice & Live Sessions": "live",
  "Tools & Extraction": "tools",
  "Composition & Patterns": "compose",
  Memory: "memory",
  Examples: "examples",
  "ADK Web UI": "web-ui",
  Reference: "reference",
};

/** Sections this script owns under src/content/docs (all gitignored). */
const SYNCED_SECTIONS = new Set(Object.values(SECTION_DIRS));

/** Pages that are curated in this app rather than synced from docs/. */
const CURATED = new Set(["introduction.md"]);

const slugify = (s) =>
  s
    .toLowerCase()
    .replace(/&/g, "and")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");

/**
 * Parse SUMMARY.md into ordered sections of pages.
 * `src` is the path relative to docs/src exactly as SUMMARY links it
 * (e.g. `user-guide/flow.md`) — the key every relative link resolves to.
 */
function parseSummary() {
  const text = readFileSync(join(DOCS_SRC, "SUMMARY.md"), "utf8");
  const sections = [];
  let current = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    const heading = /^#\s+(.+)$/.exec(line);
    if (heading && heading[1] !== "Summary") {
      current = { label: heading[1].trim(), dir: SECTION_DIRS[heading[1].trim()] ?? slugify(heading[1]), pages: [] };
      sections.push(current);
      continue;
    }
    const entry = /^-\s+\[([^\]]+)\]\(\.\/([^)]+\.md)\)/.exec(line);
    if (!entry) continue;
    if (!current) throw new Error(`SUMMARY.md: page "${entry[1]}" appears before any section heading`);
    const src = entry[2];
    if (CURATED.has(src)) continue;
    const base = posix.basename(src, ".md");
    current.pages.push({
      label: entry[1].trim(),
      src,
      dest: `${current.dir}/${base}.md`,
      route: `${SITE_BASE}/${current.dir}/${base}/`,
      order: current.pages.length + 1,
    });
  }
  return sections;
}

/** First `# ` heading → title; return { title, body } with that line removed. */
function extractTitle(body) {
  const lines = body.split("\n");
  const i = lines.findIndex((l) => /^#\s+/.test(l));
  if (i === -1) return { title: "", body };
  const title = lines[i].replace(/^#\s+/, "").trim();
  lines.splice(i, 1);
  return { title, body: lines.join("\n").replace(/^\n+/, "") };
}

/** First real paragraph, flattened to one line, for the description. */
function firstParagraph(body) {
  for (const block of body.split(/\n\s*\n/)) {
    const t = block.trim();
    if (!t || /^(#|```|\||>|<|!\[|- |\d+\. )/.test(t)) continue;
    return t.replace(/\s+/g, " ").replace(/[[\]`*_]/g, "").replace(/\([^)]*\)/g, "");
  }
  return "";
}

/** YAML-safe single-line double-quoted scalar, capped at a word boundary. */
function yamlString(s) {
  let clean = s.replace(/\s+/g, " ").trim();
  if (clean.length > 180) clean = `${clean.slice(0, 180).replace(/\s+\S*$/, "")}…`;
  return `"${clean.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

/** The on-disk file behind a docs/src path (user-guide/ and assets/ are symlinks). */
function realSource(src) {
  if (src.startsWith("user-guide/")) return `docs/${src}`;
  return `docs/src/${src}`;
}

/**
 * Rewrite every relative link and image target on a page. Targets are
 * resolved against the page's own directory to a path relative to docs/src;
 * from there: another synced page → its route; `api/…` → the rustdoc tree
 * the workflow merges into dist/api; `assets/…` → public/assets; any other
 * existing repository file → GitHub. External, absolute and fragment-only
 * links are left alone.
 */
function rewriteLinks(markdown, { src, routeMap }) {
  const pageDir = posix.dirname(src);
  const rewriteTarget = (rawTarget) => {
    const parsed = /^(\S+?)(\s+["'][\s\S]*["'])?$/.exec(rawTarget.trim());
    if (!parsed) return null;
    const [, target, title = ""] = parsed;
    if (/^(?:[a-z][a-z0-9+.-]*:|#|\/)/i.test(target)) return null;

    const cut = [target.indexOf("#"), target.indexOf("?")].filter((i) => i >= 0).sort((a, b) => a - b)[0];
    const pathPart = cut === undefined ? target : target.slice(0, cut);
    const suffix = cut === undefined ? "" : target.slice(cut);
    const fromSrc = posix.normalize(posix.join(pageDir, pathPart)); // relative to docs/src

    if (routeMap.has(fromSrc)) return `${routeMap.get(fromSrc)}${suffix}${title}`;
    if (fromSrc === "introduction.md") return `${SITE_BASE}/${suffix}${title}`;
    if (fromSrc.startsWith("api/")) return `${SITE_BASE}/${fromSrc}${suffix}${title}`;
    if (fromSrc.startsWith("assets/")) return `${SITE_BASE}/${fromSrc}${suffix}${title}`;

    // Any other repository file → GitHub. Authors write these links against
    // the page's *real* directory (docs/user-guide/ for the guide pages, which
    // docs/src reaches through a symlink), so resolve from there first and
    // from the symlink view second.
    for (const from of [posix.dirname(realSource(src)), "docs/src"]) {
      const candidate = posix.normalize(posix.join(from, from === "docs/src" ? fromSrc : pathPart));
      if (!candidate.startsWith("../") && existsSync(join(REPO_ROOT, candidate))) {
        return `${REPO_URL}/blob/main/${candidate}${suffix}${title}`;
      }
    }
    return null;
  };

  // Markdown links and images.
  let out = markdown.replace(/(!?\[[^\]]*\])\(([^)]+)\)/g, (whole, label, rawTarget) => {
    const rewritten = rewriteTarget(rawTarget);
    return rewritten ? `${label}(${rewritten})` : whole;
  });
  // Raw HTML <img src="…"> and <a href="…"> (the diagrams use <p align=center><img …>).
  out = out.replace(/(<(?:img|a)\b[^>]*?\b(?:src|href)=")([^"]+)(")/g, (whole, pre, target, post) => {
    const rewritten = rewriteTarget(target);
    return rewritten ? `${pre}${rewritten}${post}` : whole;
  });
  return out;
}

/**
 * mdBook fences carry rustdoc-style attributes — ```rust,ignore, ```rust,no_run —
 * which Expressive Code reads as a language named "rust,ignore" and gives up
 * highlighting. Keep the language, drop the attributes.
 */
function normalizeFences(markdown) {
  return markdown.replace(/^(\s*```)([A-Za-z0-9_+-]+),[^\n]*$/gm, "$1$2");
}

function transform(page, routeMap) {
  const raw = readFileSync(join(DOCS_SRC, page.src), "utf8");
  const { title: h1, body } = extractTitle(raw);
  const description = firstParagraph(body);
  const rewritten = rewriteLinks(normalizeFences(body), { src: page.src, routeMap });
  const frontmatter = [
    "---",
    `title: ${yamlString(h1 || page.label)}`,
    description ? `description: ${yamlString(description)}` : "",
    "sidebar:",
    `  label: ${yamlString(page.label)}`,
    `  order: ${page.order}`,
    `editUrl: ${REPO_URL}/edit/main/${realSource(page.src)}`,
    "---",
    "",
  ]
    .filter(Boolean)
    .join("\n");
  return `${frontmatter}\n${rewritten.trimEnd()}\n`;
}

/** Remove generated pages whose source is gone, leaving live paths for in-place rewrite. */
function removeStalePages(directory, prefix, desired) {
  let entries;
  try {
    entries = readdirSync(directory, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  for (const entry of entries) {
    const path = join(directory, entry.name);
    const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      removeStalePages(path, relative, desired);
      if (readdirSync(path).length === 0) rmSync(path);
    } else if (!desired.has(relative)) {
      rmSync(path);
    }
  }
}

/** The sidebar Starlight renders, straight from SUMMARY.md's order. */
function sidebarModule(sections) {
  const sidebar = sections.map((s) => ({
    label: s.label,
    items: s.pages.map((p) => ({ label: p.label, slug: p.dest.replace(/\.md$/, "") })),
  }));
  return [
    "// GENERATED by scripts/sync-content.mjs from docs/src/SUMMARY.md — do not edit.",
    `export const sidebar = ${JSON.stringify(sidebar, null, 2)};`,
    "",
  ].join("\n");
}

/**
 * mdBook served docs/src/foo.md at /foo.html and user-guide/foo.md at
 * /user-guide/foo.html. Every such URL that was ever linked keeps working:
 * a static stub at exactly that path, under public/, meta-refreshes to the
 * page's new route. (Astro's own `redirects` emit `foo.html/index.html` —
 * a directory — which costs an extra hop on GitHub Pages and trips
 * Pagefind; a real file at the old path is what a stale link needs.)
 */
function writeLegacyRedirects(pages) {
  const stubs = [["introduction.html", `${SITE_BASE}/`], ...pages.map((p) => [p.src.replace(/\.md$/, ".html"), p.route])];
  for (const [oldPath, route] of stubs) {
    const out = join(PUBLIC, oldPath);
    mkdirSync(dirname(out), { recursive: true });
    writeFileSync(
      out,
      `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Redirecting…</title>` +
        `<meta http-equiv="refresh" content="0;url=${route}"><meta name="robots" content="noindex">` +
        `<link rel="canonical" href="${SITE_URL}${route}"></head>` +
        `<body data-pagefind-ignore><a href="${route}">This page moved to ${route}</a></body></html>\n`,
      "utf8",
    );
  }
  return stubs.length;
}

function run() {
  const sections = parseSummary();
  const pages = sections.flatMap((s) => s.pages);
  const routeMap = new Map(pages.map((p) => [p.src, p.route]));

  if (DRY_RUN) {
    for (const p of pages) console.log(`would write ${p.dest} from ${realSource(p.src)}`);
    console.log(`[dry-run] ${pages.length} pages in ${sections.length} sections`);
    return;
  }

  // Astro's persistent content store can otherwise keep a stale digest while
  // the files are rewritten and report one path twice as a duplicate id.
  rmSync(ASTRO_CONTENT_CACHE, { force: true });
  const desired = new Set(pages.map((p) => p.dest));
  for (const section of SYNCED_SECTIONS) removeStalePages(join(CONTENT, section), section, desired);

  for (const page of pages) {
    const outPath = join(CONTENT, page.dest);
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, transform(page, routeMap), "utf8");
  }
  writeFileSync(SIDEBAR_OUT, sidebarModule(sections), "utf8");

  rmSync(PUBLIC_ASSETS, { recursive: true, force: true });
  cpSync(join(REPO_ROOT, "docs", "assets"), PUBLIC_ASSETS, { recursive: true, dereference: true });

  // Legacy stubs: clear the previous set so a renamed page leaves no orphan.
  rmSync(join(PUBLIC, "user-guide"), { recursive: true, force: true });
  for (const f of readdirSync(PUBLIC)) if (f.endsWith(".html")) rmSync(join(PUBLIC, f));
  const stubs = writeLegacyRedirects(pages);

  console.log(
    `synced ${pages.length} pages in ${sections.length} sections from docs/, plus assets and ${stubs} mdBook-era redirects`,
  );
}

run();
