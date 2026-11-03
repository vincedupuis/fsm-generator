# fsm-generator

Code generator for FSM Editor state machines (`*.fsm`: XMI 2.5.1), in Rust, with Jinja templates rendered by MiniJinja. A port of the `fsm` CLI of `../fsm-editor-vscode` (`src/cli.ts` and `src/codegen/`), with the Handlebars templates converted to Jinja. User docs: `README.md`.

## Commands

```sh
cargo build                 # target/debug/fsm-generator; `-t ts` finds templates/ through ../../templates
cargo build --release
cargo test                  # unit tests (grammar, front matter, case filters, globs, paths)
cargo clippy --all-targets
cargo fmt                   # rustfmt.toml: 120 columns
cargo run -- "../fsm-editor-vscode/examples/*.fsm" -t sml -o <dir>
```

## Architecture

One module per TypeScript file of the vscode edition, with the same logic, order of operations and messages:

| Module | Port of | |
| --- | --- | --- |
| `xml.rs`, `xmi.rs` | `src/xml.ts`, `fromXmi` in `src/xmi.ts` | Reading only, without the layout. |
| `model.rs` | `src/model.ts` | `FsmModel`; `ModelIndex` designates vertices by index in `model.vertices`. |
| `expressions.rs` | `media/expressions.js` | The text grammar. |
| `validation.rs` | `src/validation.ts` | |
| `codemodel.rs` | `src/codegen/codeModel.ts` | `to_code_model()`. Serialized with serde in camelCase: field names and order follow the TypeScript objects. |
| `tables.rs` | `src/codegen/tables.ts` | |
| `render.rs`, `languages.rs` | `src/codegen/render.ts`, `helpers.ts`, `languages.ts` | Front matter, the MiniJinja environment and its filters and functions. |
| `generate.rs` | `src/codegen/generate.ts` | Loading submachines, validating, rendering. |
| `main.rs`, `paths.rs` | `src/cli.ts` | CLI, glob expansion, `decodeURI`, path display. |

## Rules

- **Parity:** the code model must stay identical to the vscode edition's `toCodeModel()`, validation messages identical, and `templates/ts.jinja` and `templates/sml.jinja` must generate byte-identical files to `ts.hbs` and `sml.hbs`. A change to the code model, the validator or a template there needs the same change here, and the reverse. Check it with the umbrella repo's parity test, `../parity/run.py` (both generators on every model set: code models, generated files, stderr, exit codes), after changing the code model, validation, tables or a template on either side.
- Templates are data: no filter or function may run code or read files from the template's repository.
- Template-facing names (filters, functions, variables, the `file` block) are documented in `README.md`; update it when they change.
- Out of scope, as in the other editions: state machine inheritance/redefinition and the transition-oriented notation.

## Releasing

From the umbrella repo, on a clean, pushed main: `./release.sh generator <version>`. It sets the version in `Cargo.toml` and `Cargo.lock` (`-v` prints `CARGO_PKG_VERSION`), commits "Release <version>", tags `v<version>` and pushes. The tag starts `.github/workflows/release.yml`: `cargo test`, a release build per platform (`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`, `windows-x64`), each archived as `fsm-generator-<version>-<platform>.tar.gz` (`.zip` on Windows) with the executable and `templates/` next to it, attached to the GitHub Release. A tag can't be reused: if the workflow fails, fix, push and release the next patch version. Check the run with `gh run list -R vincedupuis/fsm-generator`.
