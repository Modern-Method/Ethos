## What does this PR do?

<!-- One paragraph. What changed and why. Be specific — "refactored X" is not enough. -->


## Linked Issue

<!-- PRs without a linked issue will be closed. -->
Closes #

## Type of Change

<!-- Check all that apply -->
- [ ] Bug fix
- [ ] New feature
- [ ] Refactor (no behavior change)
- [ ] Performance improvement
- [ ] Documentation
- [ ] Dependency update

## How was this tested?

<!-- Describe the tests you wrote or ran. Include commands if relevant. -->


## Test Coverage

<!-- Paste the relevant output from `cargo tarpaulin` or equivalent. Coverage must not drop below 90%. -->

```
// paste coverage output here
```

## Checklist

- [ ] I opened an issue and discussed this change before writing code
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy -- -D warnings` passes with no warnings
- [ ] All new code has tests; coverage is ≥ 90%
- [ ] I have not changed the IPC protocol in a breaking way (or I've included a migration path)
- [ ] I have not added unnecessary dependencies
- [ ] Documentation is updated if behavior changed

## Breaking Changes

<!-- Does this PR change any public APIs, IPC message formats, DB schema, or embedding dimensions? -->
- [ ] No breaking changes
- [ ] Yes — describe the impact and migration path below:

<!-- migration path / notes -->


## Additional Context

<!-- Screenshots, benchmarks, architecture notes, anything else reviewers should know. -->
