#!/usr/bin/env bash
# Checks the architecture table every packaging script and release workflow
# spells its names from.
#
# These mappings are the one place a release can be wrong in a way nothing else
# catches: a `.deb` built correctly and labeled `amd64` on an `arm64` machine
# installs nowhere, and the packaging scripts have no way to notice, because
# they ask this table what the name is. So the table is checked directly,
# against the names Debian, RPM, AppImage, and the dynamic linker actually use.
#
# Needs no toolchain, no build, and no network.
#
#   usage: packaging/test-linux-architectures.sh
set -euo pipefail

FAILURES=0

fail() {
    printf 'test-linux-architectures: %s\n' "$*" >&2
    exit 1
}

check() {
    local what=$1 expected=$2 actual=$3
    if [[ ${actual} == "${expected}" ]]; then
        printf '  ok   %s\n' "${what}"
    else
        printf '  FAIL %s: expected %s, got %s\n' "${what}" "${expected}" "${actual}" >&2
        FAILURES=$((FAILURES + 1))
    fi
}

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
PACKAGING_DIR="${REPO_ROOT}/packaging"

# shellcheck source=packaging/linux-baseline
source "${PACKAGING_DIR}/linux-baseline"
# shellcheck source=packaging/linux-architectures
source "${PACKAGING_DIR}/linux-architectures"
# shellcheck source=packaging/host-tools
source "${PACKAGING_DIR}/host-tools"

printf '==> the declared release architectures\n'
: "${EPHERVEX_BASELINE_ARCHITECTURES:?the baseline file must declare the supported architectures}"
check "the supported set" "x86_64 aarch64" "${EPHERVEX_BASELINE_ARCHITECTURES}"

printf '==> Debian names\n'
check "x86_64 is Debian amd64" amd64 "$(ephervex_deb_arch x86_64)"
check "aarch64 is Debian arm64" arm64 "$(ephervex_deb_arch aarch64)"

printf '==> RPM names\n'
check "x86_64 is RPM x86_64" x86_64 "$(ephervex_rpm_arch x86_64)"
check "aarch64 is RPM aarch64" aarch64 "$(ephervex_rpm_arch aarch64)"

printf '==> AppImage names\n'
check "x86_64 is AppImage x86_64" x86_64 "$(ephervex_appimage_arch x86_64)"
check "aarch64 is AppImage aarch64" aarch64 "$(ephervex_appimage_arch aarch64)"

printf '==> dynamic linkers\n'
check "the x86_64 linker" ld-linux-x86-64.so.2 "$(ephervex_dynamic_loader x86_64)"
check "the aarch64 linker" ld-linux-aarch64.so.1 "$(ephervex_dynamic_loader aarch64)"

printf '==> ELF machine names\n'
check "the x86_64 ELF machine" "Advanced Micro Devices X86-64" "$(ephervex_elf_machine x86_64)"
check "the aarch64 ELF machine" "AArch64" "$(ephervex_elf_machine aarch64)"

printf '==> an architecture nobody declared\n'
# The refusal is the point. A lookup that fell through to a default would give a
# name to something Ephervex does not release for, which is worse than no name.
for query in ephervex_deb_arch ephervex_rpm_arch ephervex_appimage_arch \
    ephervex_dynamic_loader ephervex_elf_machine; do
    if ("${query}" riscv64) >/dev/null 2>&1; then
        printf '  FAIL %s answered for riscv64\n' "${query}" >&2
        FAILURES=$((FAILURES + 1))
    else
        printf '  ok   %s refuses riscv64\n' "${query}"
    fi
done

printf '==> the dynamic linker is not in the flat allowlist\n'
# It is architecture-specific, so listing it there would let a binary built for
# the wrong architecture clear the check that exists to catch exactly that.
if grep -qE '^[[:space:]]*ld-linux' "${PACKAGING_DIR}/linux-shared-libraries"; then
    printf '  FAIL packaging/linux-shared-libraries names a dynamic linker\n' >&2
    FAILURES=$((FAILURES + 1))
else
    printf '  ok   the allowlist leaves the linker to the architecture table\n'
fi

printf '==> every declared architecture has pinned host tools\n'
for arch in ${EPHERVEX_BASELINE_ARCHITECTURES}; do
    ephervex_require_known_arch "${arch}"

    read -r url digest < <(ephervex_appimagetool "${arch}") ||
        fail "no appimagetool is pinned for ${arch}"
    check "appimagetool for ${arch} is named for it" \
        "appimagetool-${arch}.AppImage" "${url##*/}"
    check "the appimagetool digest for ${arch} is a SHA-256" \
        64 "${#digest}"

    read -r url digest < <(ephervex_appimage_runtime "${arch}") ||
        fail "no AppImage runtime is pinned for ${arch}"
    check "the runtime for ${arch} is named for it" "runtime-${arch}" "${url##*/}"
    check "the runtime digest for ${arch} is a SHA-256" 64 "${#digest}"
done

printf '==> the pinned digests differ per architecture\n'
# Two architectures sharing a digest means one of them was copied rather than
# fetched, which is the failure this file exists to make impossible to miss.
read -r _ x86_tool < <(ephervex_appimagetool x86_64)
read -r _ arm_tool < <(ephervex_appimagetool aarch64)
read -r _ x86_runtime < <(ephervex_appimage_runtime x86_64)
read -r _ arm_runtime < <(ephervex_appimage_runtime aarch64)
if [[ ${x86_tool} == "${arm_tool}" || ${x86_runtime} == "${arm_runtime}" ]]; then
    printf '  FAIL two architectures are pinned to one digest\n' >&2
    FAILURES=$((FAILURES + 1))
else
    printf '  ok   each architecture is pinned to its own asset\n'
fi

printf '\n'
if [[ ${FAILURES} -gt 0 ]]; then
    printf '==> %d check(s) failed\n' "${FAILURES}" >&2
    exit 1
fi
printf '==> the architecture table is what the packages need\n'
