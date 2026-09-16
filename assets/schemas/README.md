# JSON Schemas

These schemas are vendored from where they are published (fetched 2026-09-16 by `fetch_schemas.py`). The editor validates the files named in
`catalog.toml` against them; `build.rs` packs them into the program.

They are separate works, distributed alongside the program under their own
licenses, unmodified apart from line endings:

| File | Source | License |
| --- | --- | --- |
| `compose-spec.json` | <https://raw.githubusercontent.com/compose-spec/compose-spec/main/schema/compose-spec.json> | Apache-2.0 ([https://github.com/compose-spec/compose-spec](https://github.com/compose-spec/compose-spec)) |
| `github-workflow.json` | <https://json.schemastore.org/github-workflow.json> | Apache-2.0 ([https://github.com/SchemaStore/schemastore](https://github.com/SchemaStore/schemastore)) |
| `github-action.json` | <https://json.schemastore.org/github-action.json> | Apache-2.0 ([https://github.com/SchemaStore/schemastore](https://github.com/SchemaStore/schemastore)) |
| `dependabot-2.0.json` | <https://json.schemastore.org/dependabot-2.0.json> | Apache-2.0 ([https://github.com/SchemaStore/schemastore](https://github.com/SchemaStore/schemastore)) |
| `cargo.json` | <https://json.schemastore.org/cargo.json> | Apache-2.0 ([https://github.com/SchemaStore/schemastore](https://github.com/SchemaStore/schemastore)) |
| `package.json` | <https://json.schemastore.org/package.json> | Apache-2.0 ([https://github.com/SchemaStore/schemastore](https://github.com/SchemaStore/schemastore)) |
| `tsconfig.json` | <https://json.schemastore.org/tsconfig.json> | Apache-2.0 ([https://github.com/SchemaStore/schemastore](https://github.com/SchemaStore/schemastore)) |
| `gitlab-ci.json` | <https://gitlab.com/gitlab-org/gitlab/-/raw/master/app/assets/javascripts/editor/schema/ci.json> | MIT ([https://gitlab.com/gitlab-org/gitlab/-/blob/master/LICENSE](https://gitlab.com/gitlab-org/gitlab/-/blob/master/LICENSE)) |

The Apache License 2.0 text is in `LICENSE-APACHE-2.0`. The GitLab CI schema
is part of GitLab, whose license (the schema falls under its MIT Expat terms)
is in `LICENSE-GITLAB`.
