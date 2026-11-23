# Security policy

## Supported versions

The latest `0.x` release receives fixes. Older releases are not patched; the crates and the npm package publish together, so a fix is a new release.

## Reporting a vulnerability

Open a private report through GitHub's security advisory form:

<https://github.com/Litz-Sim/litz/security/advisories/new>

Do not open a public issue, and do not include exploit details in a pull request. Include the affected version, the platform, a reproduction and the impact you see. A report is acknowledged as soon as possible, and the advisory stays private until a release containing the fix is out.

## Scope

The simulator executes firmware images and WebAssembly parts. A bug that lets a part or a chip module escape its sandbox, corrupt the host process, or reach the network when the project did not ask for it is in scope. A payload that only misbehaves inside the simulated circuit, or a project that runs its own firmware, is not.
