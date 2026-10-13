# Contributing

Thank you for looking. Paleto Modkit is a personal project, mostly written with an AI assistant ([Claude Code](https://claude.com/claude-code)) and tested by hand, so a second pair of eyes is worth a lot.

## The most useful thing: tell me what fails

[Open an issue](https://github.com/the-beper/PaletoModkit/issues/new/choose). The form asks for the edition, how you installed the app and your distribution, what you did and what happened. The output of `paleto check` and, after a crash, `paleto crash`, usually saves a round of questions. [docs/faq.md](docs/faq.md#what-has-not-been-tried-in-game) lists what has not been tried in game: a report on one of those is especially welcome.

## Code and documentation

- [docs/development.md](docs/development.md) covers the project layout, building, the tests, how the screenshots are made and the packaging.
- Run `cargo clippy --all-targets -- -D warnings` and `cargo test` (also with an empty home folder, as in CI: the command is in the development page).
- The code is formatted by hand: please do not run `cargo fmt` over the tree; match the style around your change.
- Keep a change small and say why. A change that users see should come with a test and an update of the page of [docs/](docs/README.md) that describes it.
- Files of the game are not stored in the repository, and tests must not need them.

## Licence

By contributing you agree that your work is under the project's licence, GPL-3.0-or-later.
