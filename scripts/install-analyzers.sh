#!/usr/bin/env bash
#
# Fetch the pinned analyzer distributions used by `sprint-grader-static-analysis`
# and unpack them into `crates/static_analysis/vendor/`. Idempotent: re-runs
# skip already-installed versions. Set `FORCE=1` to redownload.
#
# Pinned versions match the constants in `pmd.rs::PMD_VERSION`,
# `checkstyle.rs::CHECKSTYLE_VERSION` (T3), and `spotbugs.rs::SPOTBUGS_VERSION`
# (T6). Bumping any version requires a corresponding source change.
#
# This script is the env-var fallback referenced in
# `trackdev_static_analysis_phase2_plan.md §2 (Vendoring policy)`. It runs in
# place of committing the ~150 MB of binary distributions to the repo.

set -euo pipefail

PMD_VERSION="7.7.0"
CHECKSTYLE_VERSION="10.20.0"
SPOTBUGS_VERSION="4.8.6"
FINDSECBUGS_VERSION="1.13.0"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR="${REPO_ROOT}/crates/static_analysis/vendor"

mkdir -p "${VENDOR}"

note() { printf '\033[1;34m[install-analyzers]\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[install-analyzers]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[install-analyzers]\033[0m %s\n' "$*" >&2; }

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        warn "missing required command: $1"
        exit 1
    fi
}

require_cmd curl
require_cmd unzip

fetch_zip() {
    local name="$1" version="$2" url="$3" expected_dir="$4"
    local target="${VENDOR}/${expected_dir}"
    if [[ -d "${target}" && "${FORCE:-0}" != "1" ]]; then
        ok "${name} ${version} already installed at ${target}"
        return 0
    fi
    rm -rf "${target}"
    local zip="${VENDOR}/${name}-${version}.zip"
    note "fetching ${name} ${version}"
    curl --fail --location --silent --show-error -o "${zip}" "${url}"
    note "extracting into ${VENDOR}"
    unzip -q "${zip}" -d "${VENDOR}"
    rm -f "${zip}"
    if [[ ! -d "${target}" ]]; then
        warn "expected ${target} after extraction; got:"
        ls -la "${VENDOR}" >&2
        exit 1
    fi
    ok "${name} ${version} installed at ${target}"
}

# --- PMD --------------------------------------------------------------------
fetch_zip "pmd" "${PMD_VERSION}" \
    "https://github.com/pmd/pmd/releases/download/pmd_releases%2F${PMD_VERSION}/pmd-dist-${PMD_VERSION}-bin.zip" \
    "pmd-bin-${PMD_VERSION}"

# --- Checkstyle (single jar, no zip) ----------------------------------------
CHECKSTYLE_JAR="${VENDOR}/checkstyle-${CHECKSTYLE_VERSION}-all.jar"
if [[ -f "${CHECKSTYLE_JAR}" && "${FORCE:-0}" != "1" ]]; then
    ok "checkstyle ${CHECKSTYLE_VERSION} already installed"
else
    note "fetching checkstyle ${CHECKSTYLE_VERSION}"
    curl --fail --location --silent --show-error -o "${CHECKSTYLE_JAR}" \
        "https://github.com/checkstyle/checkstyle/releases/download/checkstyle-${CHECKSTYLE_VERSION}/checkstyle-${CHECKSTYLE_VERSION}-all.jar"
    ok "checkstyle ${CHECKSTYLE_VERSION} installed at ${CHECKSTYLE_JAR}"
fi

# --- SpotBugs ---------------------------------------------------------------
fetch_zip "spotbugs" "${SPOTBUGS_VERSION}" \
    "https://github.com/spotbugs/spotbugs/releases/download/${SPOTBUGS_VERSION}/spotbugs-${SPOTBUGS_VERSION}.zip" \
    "spotbugs-${SPOTBUGS_VERSION}"

# --- FindSecBugs plugin (single jar) ----------------------------------------
FSB_JAR="${VENDOR}/findsecbugs-plugin-${FINDSECBUGS_VERSION}.jar"
if [[ -f "${FSB_JAR}" && "${FORCE:-0}" != "1" ]]; then
    ok "findsecbugs ${FINDSECBUGS_VERSION} already installed"
else
    note "fetching findsecbugs ${FINDSECBUGS_VERSION}"
    curl --fail --location --silent --show-error -o "${FSB_JAR}" \
        "https://repo1.maven.org/maven2/com/h3xstream/findsecbugs/findsecbugs-plugin/${FINDSECBUGS_VERSION}/findsecbugs-plugin-${FINDSECBUGS_VERSION}.jar"
    ok "findsecbugs ${FINDSECBUGS_VERSION} installed at ${FSB_JAR}"
fi

ok "all analyzers installed under ${VENDOR}"
