# Security

Paleto Modkit opens archives and mod packages (OIV, ZIP, RPF) that come from other people, and it writes into your game folder. Reports about that are welcome.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting: **Security → Report a vulnerability** on the [repository](https://github.com/the-beper/PaletoModkit/security/advisories/new). Say what you did, what happened and the version (`paleto --version`, or Help → About). This is a personal project, so there is no fixed response time.

Only the latest release is supported.

## What counts

- A package or archive that makes the app write, replace or delete a file outside the `mods/` folder or the places [the OIV page](docs/oiv.md#where-changes-go) allows, or change an original game archive.
- A crafted file that makes the app run code, exhaust memory or disk beyond the [limits](docs/oiv.md#limits-on-what-a-package-may-hold), or crash.
- A weakness in the release pipeline or the signed Flatpak repository ([release.md](docs/release.md)): an unsigned or downgraded update, a signing key exposed to the build.

A mod that you install and that contains a program (`.asi`, `.dll`, scripts) runs with your rights once the game loads it. That is how GTA V mods work, not a vulnerability of the app: install mods you trust.
