#!/usr/bin/env bash
# =============================================================================
# scripts/release.sh — One-command release for gemini-rs
#
# Usage:
#   ./scripts/release.sh <version>             # full release
#   ./scripts/release.sh <version> --dry-run   # preview only
#
# Flow (fully automated):
#   1.  Guard: must be on main, clean working tree, up-to-date with remote
#   2.  Validate semver; reject if version <= current
#   3.  Run full local suite: fmt, check, clippy, test
#   4.  Generate CHANGELOG entry from git log (conventional-commit aware)
#   5.  Write GITHUB_RELEASE_vX.Y.Z.md from the generated changelog
#   6.  Bump version in [workspace.package] AND [workspace.dependencies]
#       — single file (Cargo.toml), single sed pass
#   7.  Commit  "chore: release vX.Y.Z"
#   8.  Annotated tag  vX.Y.Z  (body = release notes)
#   9.  Push commit + tag  →  CI handles: validate → publish → GitHub Release
# =============================================================================
set -euo pipefail

# ── Colour helpers ─────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
info()    { echo -e "${CYAN}${BOLD}▶${RESET} $*"; }
ok()      { echo -e "${GREEN}${BOLD}✓${RESET} $*"; }
warn()    { echo -e "${YELLOW}${BOLD}⚠${RESET} $*"; }
die()     { echo -e "${RED}${BOLD}✗ ERROR:${RESET} $*" >&2; exit 1; }
step()    { echo -e "\n${BOLD}── $* ──${RESET}"; }

# ── Args ──────────────────────────────────────────────────────────────────
VERSION="${1:-}"
DRY_RUN=false
[[ "${2:-}" == "--dry-run" ]] && DRY_RUN=true

if [[ -z "$VERSION" ]]; then
  cat <<EOF
Usage: $0 <version> [--dry-run]

Examples:
  $0 0.6.0
  $0 0.6.0-rc.1 --dry-run

The version must be a valid semver string. Do not include the leading 'v'.
EOF
  exit 1
fi

VERSION="${VERSION#v}"   # strip leading v if provided
TAG="v${VERSION}"

# ── Semver validation ─────────────────────────────────────────────────────
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?(\+[a-zA-Z0-9.]+)?$'; then
  die "Invalid semver: '$VERSION'. Expected form: 1.2.3 or 1.2.3-rc.1"
fi

# ── Repo root ─────────────────────────────────────────────────────────────
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# ── Read current workspace version ────────────────────────────────────────
CURRENT=$(grep -m1 '^version = "' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')

step "Release ${CURRENT} → ${VERSION}"
$DRY_RUN && warn "DRY RUN — no files will be modified, no commits, no pushes"

# ── Guard: git state ──────────────────────────────────────────────────────
step "Git preflight"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
[[ "$BRANCH" == "main" ]] || die "Must release from 'main' (currently on '$BRANCH')."

if ! git diff --quiet || ! git diff --cached --quiet; then
  die "Working tree is dirty. Commit or stash changes before releasing."
fi

git fetch origin main --quiet
BEHIND=$(git rev-list HEAD..origin/main --count 2>/dev/null || echo 0)
[[ "$BEHIND" -gt 0 ]] && die "Branch is $BEHIND commit(s) behind origin/main. Pull first."
ok "Git state clean"

# ── Guard: tag collision ───────────────────────────────────────────────────
if git rev-parse "$TAG" >/dev/null 2>&1; then
  warn "Tag $TAG already exists."
  read -rp "  Delete and re-create? [y/N] " CONFIRM
  [[ "$CONFIRM" =~ ^[Yy]$ ]] || die "Aborted."
  $DRY_RUN || git tag -d "$TAG"
fi

# ── Validation suite ──────────────────────────────────────────────────────
step "Running validation suite"

run_cmd() {
  info "$*"
  $DRY_RUN && return
  "$@"
}

run_cmd cargo fmt --all -- --check
run_cmd cargo check --workspace --all-targets
run_cmd cargo clippy --workspace --all-targets -- -D warnings
run_cmd cargo test --workspace
ok "All checks passed"

# ── Generate changelog ────────────────────────────────────────────────────
step "Generating changelog"

PREV_TAG=$(git tag --sort=-version:refname | grep -v "^${TAG}$" | head -1 || true)
if [[ -n "$PREV_TAG" ]]; then
  RANGE="${PREV_TAG}..HEAD"
  info "Commits since $PREV_TAG"
else
  RANGE="HEAD"
  info "No previous tag found — using all commits"
fi

# Bucket commits by conventional-commit prefix
_bucket() {
  local prefix=$1; shift
  git log --oneline --no-decorate "$RANGE" 2>/dev/null \
    | grep -iE "^[a-f0-9]+ ${prefix}" | sed 's/^[a-f0-9]* /- /' || true
}

FEATS=$(_bucket "feat")
FIXES=$(_bucket "fix")
PERFS=$(_bucket "perf")
REFACTORS=$(_bucket "refactor")
DOCS=$(_bucket "docs")
CHORES=$(_bucket "chore")

_section() {
  local title=$1 body=$2
  [[ -n "$body" ]] && printf "\n### %s\n\n%s\n" "$title" "$body"
}

CHANGELOG_BODY="$(_section "Features" "$FEATS")
$(_section "Bug Fixes" "$FIXES")
$(_section "Performance" "$PERFS")
$(_section "Refactors" "$REFACTORS")
$(_section "Documentation" "$DOCS")
$(_section "Chores" "$CHORES")"

# Fallback: raw commit list if no conventional commits detected
if [[ -z "$(echo "$CHANGELOG_BODY" | tr -d '[:space:]')" ]]; then
  RAW=$(git log --oneline --no-decorate "$RANGE" 2>/dev/null | sed 's/^[a-f0-9]* /- /' || true)
  CHANGELOG_BODY="### Changes

${RAW}"
fi

TODAY=$(date +%Y-%m-%d)

# ── Write GITHUB_RELEASE file ─────────────────────────────────────────────
NOTES_FILE="GITHUB_RELEASE_v${VERSION}.md"
step "Writing $NOTES_FILE"

RELEASE_BODY="# v${VERSION} — $(date +%B\ %Y)
${CHANGELOG_BODY}

---

## Crates

| Crate | Version | Install |
|-------|---------|---------|
| [\`rs-genai\`](https://crates.io/crates/rs-genai) | ${VERSION} | \`cargo add rs-genai@${VERSION}\` |
| [\`rs-adk\`](https://crates.io/crates/rs-adk) | ${VERSION} | \`cargo add rs-adk@${VERSION}\` |
| [\`adk-rs-fluent\`](https://crates.io/crates/adk-rs-fluent) | ${VERSION} | \`cargo add adk-rs-fluent@${VERSION}\` |
| [\`adk-server-core\`](https://crates.io/crates/adk-server-core) | ${VERSION} | \`cargo add adk-server-core@${VERSION}\` |
| [\`adk-cli\`](https://crates.io/crates/adk-cli) | ${VERSION} | \`cargo install adk-cli@${VERSION}\` |

## Upgrade

\`\`\`toml
adk-rs-fluent = \"${VERSION}\"
\`\`\`

**Full Changelog**: https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CHANGELOG.md"

if ! $DRY_RUN; then
  printf '%s\n' "$RELEASE_BODY" > "$NOTES_FILE"
  ok "Wrote $NOTES_FILE"
else
  echo "$RELEASE_BODY"
fi

# ── Update CHANGELOG.md ───────────────────────────────────────────────────
step "Updating CHANGELOG.md"

CHANGELOG_ENTRY="## [${VERSION}] - ${TODAY}
${CHANGELOG_BODY}"

if ! $DRY_RUN; then
  if grep -q "^## \[Unreleased\]" CHANGELOG.md; then
    # Insert new version section after [Unreleased]
    ESCAPED=$(printf '%s\n' "$CHANGELOG_ENTRY" | sed 's/[&/\]/\\&/g; s/$/\\/')
    sed -i "s/^## \[Unreleased\]/## [Unreleased]\n\n${ESCAPED}/" CHANGELOG.md
  else
    warn "No [Unreleased] section found — prepending to CHANGELOG.md"
    { echo "$CHANGELOG_ENTRY"; echo ""; cat CHANGELOG.md; } > CHANGELOG.md.tmp
    mv CHANGELOG.md.tmp CHANGELOG.md
  fi
  ok "CHANGELOG.md updated"
fi

# ── Bump version — single file, single sed pass ───────────────────────────
step "Bumping version: $CURRENT → $VERSION (Cargo.toml only)"
# [workspace.package] version = "..." and [workspace.dependencies] version = "..."
# all live in root Cargo.toml. One sed call handles both.

if ! $DRY_RUN; then
  sed -i "s/version = \"${CURRENT}\"/version = \"${VERSION}\"/g" Cargo.toml
  FOUND=$(grep -c "\"${VERSION}\"" Cargo.toml || true)
  [[ "$FOUND" -ge 2 ]] || die "Version bump landed only $FOUND time(s) — expected ≥2"
  ok "Cargo.toml bumped ($FOUND occurrences)"
fi

# ── Commit ────────────────────────────────────────────────────────────────
step "Committing"
if ! $DRY_RUN; then
  git add Cargo.toml CHANGELOG.md "$NOTES_FILE"
  git commit -m "chore: release ${TAG}"
  ok "Committed: chore: release ${TAG}"
fi

# ── Annotated tag ─────────────────────────────────────────────────────────
step "Tagging $TAG"
if ! $DRY_RUN; then
  git tag -a "$TAG" -m "Release ${TAG}

${RELEASE_BODY}"
  ok "Tagged $TAG"
fi

# ── Push ─────────────────────────────────────────────────────────────────
step "Pushing to origin"
if ! $DRY_RUN; then
  git push origin main
  git push origin "$TAG"
  ok "Pushed commit and tag"
fi

# ── Summary ───────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
echo -e "${GREEN}${BOLD}  Released ${TAG}${RESET}"
echo ""
if ! $DRY_RUN; then
  echo -e "  CI is running at:"
  echo -e "  ${CYAN}https://github.com/vamsiramakrishnan/gemini-rs/actions${RESET}"
  echo ""
  echo -e "  Steps:"
  echo -e "  ${BOLD}1. validate${RESET}  fmt + test + clippy"
  echo -e "  ${BOLD}2. publish${RESET}   rs-genai → rs-adk → adk-rs-fluent → adk-server-core → adk-cli"
  echo -e "  ${BOLD}3. release${RESET}   GitHub Release from ${NOTES_FILE}"
fi
echo -e "${GREEN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
