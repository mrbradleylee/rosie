#!/usr/bin/env sh
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/release.sh <version> [--push] [--dry-run]

Prepares a Rosie release from main by:
- verifying the git working tree is clean
- updating Cargo.toml to the requested version
- renaming the Unreleased changelog section to the requested version
- creating a fresh Unreleased section at the top of CHANGELOG.md
- running cargo fmt and cargo check --locked
- creating a release commit and annotated tag

Use --push to push both main and the tag after the local release commit is made.
Use --dry-run to validate the release and preview edits without changing files,
creating commits, tags, or pushes.
EOF
}

die() {
    printf 'release error: %s\n' "$1" >&2
    exit 1
}

require_clean_tree() {
    git diff --quiet --ignore-submodules HEAD -- || die "working tree is not clean"
}

require_main_branch() {
    branch="$(git branch --show-current)"
    [ "$branch" = "main" ] || die "releases must be cut from main (current: $branch)"
}

require_unreleased_section() {
    if ! grep -Eq '^## (\[)?Unreleased(\])?$' CHANGELOG.md; then
        die "CHANGELOG.md must contain an Unreleased section"
    fi
}

require_version_format() {
    case "$1" in
        [0-9]*.[0-9]*.[0-9]*)
            ;;
        *)
            die "version must look like X.Y.Z"
            ;;
    esac
}

require_missing_tag() {
    if git rev-parse -q --verify "refs/tags/v$1" >/dev/null 2>&1; then
        die "tag v$1 already exists"
    fi
}

update_cargo_version() {
    version="$1"
    file="${2:-Cargo.toml}"
    perl -0pi -e 's/^version = "\K[^"]+/'"$version"'/m' "$file"
    grep -q '^version = "'"$version"'"$' "$file" || die "failed to update Cargo.toml"
}

update_changelog() {
    version="$1"
    output_file="${2:-}"
    tmp_file="${output_file:-$(mktemp)}"
    awk -v version="$version" '
        BEGIN {
            replaced = 0
        }
        /^## \[?Unreleased\]?$/ && !replaced {
            print "## Unreleased"
            print ""
            print "## " version
            replaced = 1
            next
        }
        { print }
        END {
            if (!replaced) {
                exit 2
            }
        }
    ' CHANGELOG.md >"$tmp_file" || {
        [ -n "$output_file" ] || rm -f "$tmp_file"
        die "failed to update CHANGELOG.md"
    }
    if [ -z "$output_file" ]; then
        mv "$tmp_file" CHANGELOG.md
    fi
}

require_release_notes() {
    version="$1"
    file="${2:-CHANGELOG.md}"
    notes="$(
        awk -v version="$version" '
            $0 ~ "^## \\[" version "\\]$" || $0 ~ "^## " version "$" { found=1; next }
            found && /^## / { exit }
            found { print }
        ' "$file"
    )"
    [ -n "$(printf '%s' "$notes" | tr -d '[:space:]')" ] || die "release section for $version is empty"
}

main() {
    [ $# -ge 1 ] || {
        usage
        exit 1
    }

    version=''
    push=0
    dry_run=0

    while [ $# -gt 0 ]; do
        case "$1" in
            --push)
                push=1
                ;;
            --dry-run)
                dry_run=1
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                if [ -n "$version" ]; then
                    die "only one version argument is allowed"
                fi
                version="$1"
                ;;
        esac
        shift
    done

    [ -n "$version" ] || die "missing version argument"

    require_version_format "$version"
    require_clean_tree
    require_main_branch
    require_unreleased_section
    require_missing_tag "$version"

    if [ "$dry_run" -eq 1 ]; then
        cargo_tmp="$(mktemp)"
        changelog_tmp="$(mktemp)"
        cp Cargo.toml "$cargo_tmp"
        update_cargo_version "$version" "$cargo_tmp"
        update_changelog "$version" "$changelog_tmp"
        require_release_notes "$version" "$changelog_tmp"
        rm -f "$cargo_tmp" "$changelog_tmp"
    else
        update_cargo_version "$version"
        update_changelog "$version"
        require_release_notes "$version"
    fi

    cargo fmt
    cargo check --locked

    if [ "$dry_run" -eq 1 ]; then
        printf 'Dry run successful for v%s\n' "$version"
        printf 'Would update Cargo.toml and CHANGELOG.md, commit "Release v%s", and create tag v%s\n' "$version" "$version"
        if [ "$push" -eq 1 ]; then
            printf 'Would also push main and tag v%s\n' "$version"
        fi
        exit 0
    fi

    git add Cargo.toml CHANGELOG.md
    git commit -m "Release v$version"
    git tag -a "v$version" -m "Release v$version"

    if [ "$push" -eq 1 ]; then
        git push origin main
        git push origin "v$version"
    fi

    printf 'Prepared release v%s\n' "$version"
    if [ "$push" -eq 1 ]; then
        printf 'Pushed main and tag v%s\n' "$version"
    else
        printf 'Next steps:\n'
        printf '  git push origin main\n'
        printf '  git push origin v%s\n' "$version"
    fi
}

main "$@"
