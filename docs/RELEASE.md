# Release process

The release workflow is tag-driven and intentionally does not create tags.

1. Ensure CI passes on the intended release commit.
2. Review `Cargo.lock`, `cargo deny check`, action SHA pins, and the release notes
   at `docs/release-notes-v<version>.md`. The workflow fails if the file matching
   the tag name does not exist.
3. Create and push an annotated `v<version>` tag from the reviewed commit.
4. The workflow builds the two supported targets, packages `hav`, `README.md`,
   and `LICENSE`, generates `SHA256SUMS`, creates provenance attestations, and
   creates the GitHub release from the existing tag.
5. Download both archives, verify checksums and attestations, and smoke-test
   `hav --version` plus a fixture on each platform.

Artifact names are deterministic:

```text
hexagonal-architecture-validator-v<version>-<target>.tar.gz
```

The release workflow has no `pull_request` or branch trigger and cannot publish
without a `v*` tag. CI separately builds and packages the Linux target, creates
`SHA256SUMS`, and exercises the checksum command documented in
`docs/INSTALLATION.md` without publishing. GitHub Actions are pinned to
immutable commit SHAs. Jobs use
least-privilege permissions; only the final release job receives contents,
attestation, artifact-metadata, and OIDC write permissions.
