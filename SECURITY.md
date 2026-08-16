# Security policy

## Supported versions

The latest released minor version receives security fixes. Before the first
tagged release, report issues against `main`.

## Reporting a vulnerability

Use GitHub's private security advisory flow for
`nfma/hexagonal-architecture-validator`. Do not open a public issue containing
exploit details or secrets. Include the affected version, reproduction steps,
impact, and any suggested mitigation.

The validator analyzes untrusted source as data and must not execute project
build scripts, macros, or binaries. A report that shows analysis causing code
execution, network writes, or repository mutation is security-sensitive.
