<!--
Developer template — open with ?template=developer.md on the compare page.
Fill in every section; incomplete PRs will be sent back for details.
Commits must follow Conventional Commits: <type>(<scope>): <Chinese description>
-->

## Summary

<!-- What does this PR do and why? Note any design trade-offs. -->

## Type of change

- [ ] `feat` — new feature
- [ ] `fix` — bug fix
- [ ] `docs` — documentation / knowledge base
- [ ] `refactor` — no external behavior change
- [ ] `perf` — performance
- [ ] `test` — tests
- [ ] `build` — build / dependencies
- [ ] `ci` — CI / toolchain
- [ ] `chore` — maintenance

## Related issues

<!-- e.g. Closes #12; otherwise "None" -->

Closes #

## How to test

<!-- Steps a reviewer can run to reproduce and verify the change -->

1.
2.

## Verification

<!-- Paste commands and key output; mark N/A with a reason where not applicable -->

| Check | Command | Result |
|---|---|---|
| Build | `cargo build` | |
| Tests | `cargo test` | |
| Lints | `cargo clippy --all-targets` | |
| Format | `cargo fmt --check` | |

## Checklist (required)

- [ ] Commit messages follow `<type>(<scope>): <Chinese description>` (enforced by the commit-msg hook)
- [ ] Each commit is a single, self-contained, understandable change
- [ ] Behavior changes are covered by tests (where applicable)
- [ ] `CHANGELOG.md` updated for user-visible changes (`feat` → Added, `fix` → Fixed)
- [ ] Knowledge base updated under `kb/SeanbotTools/` when tool behavior changes
- [ ] No secrets, tokens, or private data included
- [ ] Nothing excluded by `.gitignore` (e.g. `target/`, local `docs/`) is included

## Breaking changes

<!-- Describe impact and migration path if any; otherwise "None" -->

None

## Screenshots / recordings

<!-- For CLI output, animations, or interactive changes -->

## Notes for reviewers

<!-- Focus areas, known limitations, follow-ups -->
