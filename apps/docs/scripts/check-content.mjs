#!/usr/bin/env node
// Validates the synced content tree before Astro builds it, so a broken page
// fails with a file:line rather than a Starlight stack trace:
//   - every page has a frontmatter title
//   - every site-internal link (`/gemini-rs/…`) resolves to a page this build
//     will emit, to the rustdoc tree the workflow merges in (`/api/…`), or to
//     a file under public/
// This is the rendered-tree complement to the lychee run on docs/ sources.
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { SITE_BASE } from "../src/lib/site-meta.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const APP = join(HERE, "..");
const CONTENT = join(APP, "src", "content", "docs");
const PUBLIC = join(APP, "public");

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(path));
    else if (/\.mdx?$/.test(entry.name)) out.push(path);
  }
  return out;
}

const pages = walk(CONTENT);
const routes = new Set(
  pages.map((p) => {
    const rel = relative(CONTENT, p).replace(/\\/g, "/").replace(/\.mdx?$/, "");
    return rel === "index" ? `${SITE_BASE}/` : `${SITE_BASE}/${rel}/`;
  }),
);

/** Every regex metacharacter, backslash included — SITE_BASE is config, not a pattern. */
const escapeRegExp = (s) => s.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");

const problems = [];
for (const page of pages) {
  const text = readFileSync(page, "utf8");
  const rel = relative(APP, page);
  if (!/^---\n[\s\S]*?^title:\s*\S/m.test(text)) problems.push(`${rel}: no frontmatter title`);

  const linkRe = new RegExp(`(?:\\]\\(|src="|href=")(${escapeRegExp(SITE_BASE)}\\/[^)"#?\\s]*)`, "g");
  for (const m of text.matchAll(linkRe)) {
    const target = m[1];
    if (target.startsWith(`${SITE_BASE}/api/`)) continue; // merged in by the workflow
    if (routes.has(target)) continue;
    const asFile = join(PUBLIC, target.slice(SITE_BASE.length + 1));
    if (existsSync(asFile) && statSync(asFile).isFile()) continue;
    const line = text.slice(0, m.index).split("\n").length;
    problems.push(`${rel}:${line}: link to ${target} matches no page, asset, or api route`);
  }
}

if (problems.length) {
  console.error(`check-content: ${problems.length} problem(s)\n  ${problems.join("\n  ")}`);
  process.exit(1);
}
console.log(`check-content: ${pages.length} pages, every internal link resolves`);
