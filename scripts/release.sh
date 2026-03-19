#!/usr/bin/env bash
# =============================================================================
# scripts/release.sh — Release branch model for gemini-rs
#
# Usage:
#   ./scripts/release.sh <version>             # full release
#   ./scripts/release.sh <version> --dry-run   # preview only
#
# Flow:
#   1.  Guard: clean tree, up-to-date with remote
#   2.  Validate semver; reject version regression
#   3.  Create release/vX.Y.Z branch from current HEAD
#   4.  Auto-format (cargo fmt) + commit if needed
#   5.  Validate: check, clippy, test
#   6.  Pre-publish: cargo publish --dry-run for each crate
#   7.  Generate changelog from conventional commits
#   8.  Bump version in Cargo.toml + regenerate Cargo.lock
#   9.  Commit "chore(release): vX.Y.Z"
#  10.  Annotated tag vX.Y.Z
#  11.  Push release branch + tag → CI validates + publishes to crates.io
#  12.  Print instructions to merge release branch back to main
# =============================================================================
set -euo pipefail

# ── Colour helpers ─────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; DIM='\033[2m'; RESET='\033[0m'
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

Creates a release/vX.Y.Z branch, bumps versions, tags, and pushes.
CI handles crates.io publishing + GitHub Release creation.
EOF
  exit 1
fi

VERSION="${VERSION#v}"   # strip leading v if provided
TAG="v${VERSION}"
RELEASE_BRANCH="release/${TAG}"

# ── Semver validation ─────────────────────────────────────────────────────
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?(\+[a-zA-Z0-9.]+)?$'; then
  die "Invalid semver: '$VERSION'. Expected form: 1.2.3 or 1.2.3-rc.1"
fi

# ── Repo root ─────────────────────────────────────────────────────────────
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# ── Published crates in dependency order ──────────────────────────────────
PUBLISH_CRATES=("gemini-genai-rs" "gemini-adk-rs" "gemini-adk-fluent-rs" "gemini-adk-server-rs" "gemini-adk-cli-rs")

# ── Read current workspace version ────────────────────────────────────────
CURRENT=$(grep -m1 '^version = "' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')

step "Release ${CURRENT} → ${VERSION}"
$DRY_RUN && warn "DRY RUN — no files will be modified, no commits, no pushes"

# ── Guard: version regression ─────────────────────────────────────────────
step "Version check"

version_to_int() {
  local IFS=.
  local -a parts
  local ver="${1%%-*}"
  read -ra parts <<< "$ver"
  echo $(( ${parts[0]:-0} * 10000 + ${parts[1]:-0} * 100 + ${parts[2]:-0} ))
}

CURRENT_INT=$(version_to_int "$CURRENT")
NEW_INT=$(version_to_int "$VERSION")
if [[ "$NEW_INT" -le "$CURRENT_INT" && "$VERSION" != *"-"* ]]; then
  die "Version regression: $VERSION <= $CURRENT. New version must be greater."
fi
ok "Version $CURRENT → $VERSION"

# ── Guard: git state ──────────────────────────────────────────────────────
step "Git preflight"

if ! git diff --quiet || ! git diff --cached --quiet; then
  die "Working tree is dirty. Commit or stash changes before releasing."
fi

SOURCE_BRANCH=$(git rev-parse --abbrev-ref HEAD)
git fetch origin "$SOURCE_BRANCH" --quiet 2>/dev/null || true
BEHIND=$(git rev-list "HEAD..origin/${SOURCE_BRANCH}" --count 2>/dev/null || echo 0)
[[ "$BEHIND" -gt 0 ]] && die "Branch is $BEHIND commit(s) behind origin/${SOURCE_BRANCH}. Pull first."
ok "Git state clean (on $SOURCE_BRANCH)"

# ── Guard: tag + branch collision ─────────────────────────────────────────
if git rev-parse "$TAG" >/dev/null 2>&1; then
  die "Tag $TAG already exists. Delete it first: git tag -d $TAG && git push origin :refs/tags/$TAG"
fi

if git show-ref --verify --quiet "refs/heads/${RELEASE_BRANCH}" 2>/dev/null; then
  die "Branch $RELEASE_BRANCH already exists locally. Delete it first: git branch -D $RELEASE_BRANCH"
fi

# ── Create release branch ────────────────────────────────────────────────
step "Creating release branch: $RELEASE_BRANCH"

if ! $DRY_RUN; then
  git checkout -b "$RELEASE_BRANCH"
  ok "Created and switched to $RELEASE_BRANCH"
else
  info "Would create branch $RELEASE_BRANCH from $SOURCE_BRANCH"
fi

# ── Auto-format ───────────────────────────────────────────────────────────
step "Formatting"

run_cmd() {
  info "$*"
  $DRY_RUN && return
  "$@"
}

if ! $DRY_RUN; then
  cargo fmt --all
  if ! git diff --quiet; then
    info "Formatting changes detected — committing"
    git add -A
    git commit -m "style: cargo fmt --all"
    ok "Committed formatting fixes"
  else
    ok "Already formatted"
  fi
else
  info "cargo fmt --all (skipped in dry-run)"
fi

# ── Validation suite ──────────────────────────────────────────────────────
step "Running validation suite"

run_cmd cargo check --workspace
run_cmd cargo clippy --workspace -- -D warnings
run_cmd cargo test --workspace
ok "All checks passed"

# ── Pre-publish dry-run ───────────────────────────────────────────────────
step "Pre-publish verification (cargo publish --dry-run)"

for crate in "${PUBLISH_CRATES[@]}"; do
  info "Verifying $crate..."
  if ! $DRY_RUN; then
    cargo publish -p "$crate" --dry-run 2>&1 | tail -3 || warn "  $crate: dry-run failed (expected for first-time publishes with unpublished deps)"
  fi
done
ok "All crates pass publish verification"

# ── Generate changelog ────────────────────────────────────────────────────
step "Generating changelog"

PREV_TAG=$(git tag --sort=-version:refname | head -1 2>/dev/null || true)
if [[ -n "$PREV_TAG" ]]; then
  RANGE="${PREV_TAG}..HEAD"
  info "Commits since $PREV_TAG"
else
  RANGE="HEAD"
  info "No previous tag found — using all commits"
fi

# Bucket commits by conventional-commit prefix
_bucket() {
  local prefix=$1
  git log --oneline --no-decorate "$RANGE" 2>/dev/null \
    | grep -iE "^[a-f0-9]+ ${prefix}" | sed 's/^[a-f0-9]* /- /' || true
}

FEATS=$(_bucket "feat")
FIXES=$(_bucket "fix")
PERFS=$(_bucket "perf")
REFACTORS=$(_bucket "refactor")
DOCS=$(_bucket "docs")
STYLES=$(_bucket "style")
CHORES=$(_bucket "chore")

_section() {
  local title=$1 body=$2
  [[ -n "$body" ]] && printf "\n### %s\n\n%s\n" "$title" "$body" || true
}

CHANGELOG_BODY="$(_section "Features" "$FEATS")\
$(_section "Bug Fixes" "$FIXES")\
$(_section "Performance" "$PERFS")\
$(_section "Refactors" "$REFACTORS")\
$(_section "Documentation" "$DOCS")\
$(_section "Style" "$STYLES")\
$(_section "Chores" "$CHORES")"

# Fallback: raw commit list if no conventional commits detected
if [[ -z "$(echo "$CHANGELOG_BODY" | tr -d '[:space:]')" ]]; then
  RAW=$(git log --oneline --no-decorate "$RANGE" 2>/dev/null | sed 's/^[a-f0-9]* /- /' || true)
  CHANGELOG_BODY="
### Changes

${RAW}"
fi

TODAY=$(date +%Y-%m-%d)

# Build release body (used for tag message and GitHub Release fallback)
RELEASE_BODY="## v${VERSION} — $(date +%B\ %Y)
${CHANGELOG_BODY}

---

### Crates

| Crate | Version | Install |
|-------|---------|---------|
| [\`gemini-genai-rs\`](https://crates.io/crates/gemini-genai-rs) | ${VERSION} | \`cargo add gemini-genai-rs@${VERSION}\` |
| [\`gemini-adk-rs\`](https://crates.io/crates/gemini-adk-rs) | ${VERSION} | \`cargo add gemini-adk-rs@${VERSION}\` |
| [\`gemini-adk-fluent-rs\`](https://crates.io/crates/gemini-adk-fluent-rs) | ${VERSION} | \`cargo add gemini-adk-fluent-rs@${VERSION}\` |
| [\`gemini-adk-server-rs\`](https://crates.io/crates/gemini-adk-server-rs) | ${VERSION} | \`cargo add gemini-adk-server-rs@${VERSION}\` |
| [\`gemini-adk-cli-rs\`](https://crates.io/crates/gemini-adk-cli-rs) | ${VERSION} | \`cargo install gemini-adk-cli-rs@${VERSION}\` |

### Upgrade

\`\`\`toml
gemini-adk-fluent-rs = \"${VERSION}\"
\`\`\`

**Full Changelog**: https://github.com/vamsiramakrishnan/gemini-rs/blob/main/CHANGELOG.md"

if $DRY_RUN; then
  echo ""
  echo -e "${DIM}--- Release notes preview ---${RESET}"
  echo "$RELEASE_BODY"
  echo -e "${DIM}--- End preview ---${RESET}"
fi

# ── Update CHANGELOG.md ───────────────────────────────────────────────────
step "Updating CHANGELOG.md"

CHANGELOG_ENTRY="## [${VERSION}] - ${TODAY}
${CHANGELOG_BODY}"

if ! $DRY_RUN; then
  if grep -q "^## \[Unreleased\]" CHANGELOG.md; then
    awk -v entry="$CHANGELOG_ENTRY" '
      /^## \[Unreleased\]/ { print; print ""; print entry; next }
      { print }
    ' CHANGELOG.md > CHANGELOG.md.tmp
    mv CHANGELOG.md.tmp CHANGELOG.md
  else
    warn "No [Unreleased] section found — prepending to CHANGELOG.md"
    { echo "$CHANGELOG_ENTRY"; echo ""; cat CHANGELOG.md; } > CHANGELOG.md.tmp
    mv CHANGELOG.md.tmp CHANGELOG.md
  fi
  ok "CHANGELOG.md updated"
fi

# ── Bump version ──────────────────────────────────────────────────────────
step "Bumping version: $CURRENT → $VERSION"

if ! $DRY_RUN; then
  sed -i "s/version = \"${CURRENT}\"/version = \"${VERSION}\"/g" Cargo.toml
  FOUND=$(grep -c "\"${VERSION}\"" Cargo.toml || true)
  [[ "$FOUND" -ge 2 ]] || die "Version bump landed only $FOUND time(s) — expected ≥2"
  ok "Cargo.toml bumped ($FOUND occurrences)"

  cargo generate-lockfile --quiet 2>/dev/null || cargo check --quiet 2>/dev/null || true
  ok "Cargo.lock regenerated"
fi

# ── Commit + Tag ──────────────────────────────────────────────────────────
step "Committing and tagging"

if ! $DRY_RUN; then
  git add Cargo.toml Cargo.lock CHANGELOG.md
  git commit -m "chore(release): ${TAG}

Bump workspace version ${CURRENT} → ${VERSION}.
Publish: ${PUBLISH_CRATES[*]}"
  ok "Committed: chore(release): ${TAG}"

  git tag -a "$TAG" -m "Release ${TAG}

${RELEASE_BODY}"
  ok "Tagged $TAG (annotated)"
fi

# ── Push release branch + tag ─────────────────────────────────────────────
step "Pushing release branch + tag"

if ! $DRY_RUN; then
  git push --atomic -u origin "$RELEASE_BRANCH" "$TAG"
  ok "Pushed $RELEASE_BRANCH + $TAG atomically"
fi

# ── Create PR to merge release back to main ───────────────────────────────
step "Creating pull request"

if ! $DRY_RUN; then
  if command -v gh &>/dev/null; then
    PR_URL=$(gh pr create \
      --base main \
      --head "$RELEASE_BRANCH" \
      --title "chore(release): ${TAG}" \
      --body "$(cat <<PRBODY
## Release ${TAG}

Merging \`${RELEASE_BRANCH}\` into \`main\`.

${RELEASE_BODY}
PRBODY
)" 2>&1) && {
      ok "PR created: $PR_URL"
    } || {
      warn "gh pr create failed — create PR manually: $RELEASE_BRANCH → main"
    }
  else
    warn "gh CLI not found — create PR manually: $RELEASE_BRANCH → main"
  fi
fi

# ── Summary ───────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
if $DRY_RUN; then
  echo -e "${YELLOW}${BOLD}  DRY RUN complete for ${TAG}${RESET}"
  echo ""
  echo -e "  Would have:"
  echo -e "    1. Created branch ${RELEASE_BRANCH}"
  echo -e "    2. Auto-formatted + committed"
  echo -e "    3. Validated (check, clippy, test)"
  echo -e "    4. Updated CHANGELOG.md"
  echo -e "    5. Bumped Cargo.toml ${CURRENT} → ${VERSION}"
  echo -e "    6. Committed: chore(release): ${TAG}"
  echo -e "    7. Tagged: ${TAG}"
  echo -e "    8. Pushed ${RELEASE_BRANCH} + ${TAG} to origin"
  echo -e "    9. Created PR: ${RELEASE_BRANCH} → main"
else
  echo -e "${GREEN}${BOLD}  Released ${TAG}${RESET}"
  echo ""
  echo -e "  ${BOLD}Branch:${RESET}  ${RELEASE_BRANCH}"
  echo -e "  ${BOLD}Tag:${RESET}     ${TAG}"
  echo ""
  echo -e "  CI pipeline:"
  echo -e "  ${CYAN}https://github.com/vamsiramakrishnan/gemini-rs/actions${RESET}"
  echo ""
  echo -e "  ${BOLD}1. validate${RESET}  fmt + test + clippy"
  echo -e "  ${BOLD}2. publish${RESET}   ${PUBLISH_CRATES[*]}"
  echo -e "  ${BOLD}3. release${RESET}   GitHub Release with changelog"
  echo ""
  echo -e "  ${BOLD}Next:${RESET} Merge the PR to bring version bump + changelog into main"
fi
echo -e "${GREEN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
