# Pull Requests

When all commits on a branch are done, `make check` passes, and the review agent has reported back, push and open a PR automatically.

There is no `make test-prove` in this repository; it lives in proveno-zk. The gateway does not prove anything, but it does fix what a later proof would commit to: the SHA-256 program hash and the recorded tool call log. If a change needs proveno-core to change (the oracle tape, canonical serialization, the program hash), make that change in proveno-core, run `make test-prove` in a proveno-zk checkout against it, and bump the tag here only after it is released.

- **Target:** always `main`
- **State:** always open as **draft**
- **Title:** `type(scope): short description`, the same convention as the commit that drove the work (see `.claude/rules/commits.md`)
- **Body:** summarise what changed (bullet points from the commits) and reference the spec section or agent prompt step the work came from

```bash
git push -u origin <branch>
gh pr create --draft --base main --title "..." --body "..."
```

## Agent Run Report (PR comment)

Immediately after the PR is created, post an agent run report as a PR comment. Assemble it from:
1. `git log main..HEAD --oneline`: the implementation commits
2. The review agent's returned report (captured earlier)

```bash
gh pr comment <PR-number> --body "$(cat <<'EOF'
## Agent Run Report

### Implementation Commits
- <commit hash> <commit message>
- ...

### Review Report
<paste the review agent's full structured output here>
EOF
)"
```

This comment is the permanent record of what every agent did on this branch. It must be posted before the branch is considered done.
