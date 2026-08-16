# Installation and verification

The first release publishes these archives:

- `hexagonal-architecture-validator-v0.1.0-aarch64-apple-darwin.tar.gz`
- `hexagonal-architecture-validator-v0.1.0-x86_64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

Replace `v0.1.0` below only when intentionally upgrading a pinned installation.

## macOS Apple Silicon

```console
version=v0.1.0
artifact="hexagonal-architecture-validator-${version}-aarch64-apple-darwin.tar.gz"
base="https://github.com/nfma/hexagonal-architecture-validator/releases/download/${version}"
curl --proto '=https' --tlsv1.2 -fLO "$base/$artifact"
curl --proto '=https' --tlsv1.2 -fLO "$base/SHA256SUMS"
grep "  $artifact\$" SHA256SUMS | shasum -a 256 -c -
tar -xzf "$artifact"
install -m 0755 hav /usr/local/bin/hav
hav --version
```

Use a user-owned directory on `PATH`, or `sudo install`, if `/usr/local/bin` is
not writable.

## Linux x86-64

```console
version=v0.1.0
artifact="hexagonal-architecture-validator-${version}-x86_64-unknown-linux-gnu.tar.gz"
base="https://github.com/nfma/hexagonal-architecture-validator/releases/download/${version}"
curl --proto '=https' --tlsv1.2 -fLO "$base/$artifact"
curl --proto '=https' --tlsv1.2 -fLO "$base/SHA256SUMS"
grep "  $artifact\$" SHA256SUMS | sha256sum -c -
tar -xzf "$artifact"
install -m 0755 hav /usr/local/bin/hav
hav --version
```

## Provenance

Release archives receive GitHub artifact attestations. With GitHub CLI:

```console
gh attestation verify "$artifact" --repo nfma/hexagonal-architecture-validator
```

Checksum verification confirms the downloaded bytes match the release manifest;
attestation verification additionally ties the artifact digest to the GitHub
Actions build identity.
