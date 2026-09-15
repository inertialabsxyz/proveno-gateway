# Commits

**Gate:** `make check` must pass before every commit. No exceptions.

**Auto-commit:** Commit each logical change as it is completed, without waiting to be asked. Use judgment to determine when a change is coherent and complete. Do not commit mid-feature or bundle unrelated changes.

**Message format:** `type(scope): short description`

- `type`: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`
- `scope`: the module or area. Common scopes:
  - Modules: `config`, `downstream`, `policy`, `schema`, `values`, `dialect`, `description`, `trace`, `store`, `host`, `engine`, `server`, `replay`
  - Other areas: `cli`, `demo`
  - Cross-cutting: `tests`, `docs`, `repo`, `deps`, `ci`
- Description: imperative, lowercase, no period. 72 characters total max.

```
feat(policy): enforce per-argument max constraints
fix(values): map out-of-range integers to decimal strings
feat(replay): report the first divergent call by sequence number
test(engine): assert credentials never reach the trace store
chore(deps): pin proveno-core to v0.3.0
```

**Scope:** One logical change per commit. Don't bundle unrelated fixes.
