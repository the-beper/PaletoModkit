# Flathub

1. **Tag the release**: create the tag `v1.2.0` on commit `038f812` and push it. The manifest (`packaging/io.github.the_beper.PaletoModkit.yml`) already points to that tag and commit, and the build fails until the tag exists on `origin`.
2. **AI disclosure** in the Flathub PR: the code, the documentation and the packaging were generated with AI, with an indication of which parts and how much. It must be written by you.
3. **Submission**:
   - fork `flathub/flathub` (do not copy only the default branch);
   - create a branch from the base `new-pr` with `io.github.the_beper.PaletoModkit.yml`, `flathub.json` and the two source JSON files;
   - open the PR against `new-pr` with the title "Add io.github.the_beper.PaletoModkit".
4. **After acceptance**: accept the invitation to the `flathub/io.github.the_beper.PaletoModkit` repository, and verify ownership of the `io.github.*` ID in the Flathub dashboard, logged in as `the-beper`.
