# fsm-generator

Generates code from [FSM Editor](https://github.com/vincedupuis/fsm-editor-vscode) state machines (`.fsm` files: UML 2.5.1 in XMI 2.5.1) with [Jinja](https://jinja.palletsprojects.com/) templates. One template can write any number of files, in any language.

It is the `fsm` code generator of FSM Editor, written in Rust, with templates rendered by [MiniJinja](https://github.com/mitsuhiko/minijinja) instead of Handlebars: the same validation and messages, and the same code model, the language-neutral description of a machine that templates print. The bundled `ts.jinja` and `sml.jinja` generate the same files as FSM Editor's `ts.hbs` and `sml.hbs`.

- [Building](#building)
- [Usage](#usage)
- [Bundled templates](#bundled-templates)
- [Writing a template](#writing-a-template)
- [Coming from the Handlebars templates](#coming-from-the-handlebars-templates)

## Building

```sh
cargo build --release      # target/release/fsm-generator
cargo test
```

The program is a single executable without dependencies. Templates are files, not built into it: ship it with the `templates/` folder next to it, where `-t <name>` finds them.

Prebuilt executables for Linux and macOS (x64, arm64) and Windows (x64), each with its `templates/` folder, are on the [Releases](https://github.com/vincedupuis/fsm-generator/releases) page.

## Usage

```sh
fsm-generator <files...> --template <template> [--out <folder>]
```

| Option | |
| --- | --- |
| `-t, --template <file>` | Template file. A bare name such as `ts` means `templates/ts.jinja` next to the program (or, for `cargo run`, in this folder). |
| `-o, --out <folder>` | Output folder (default: current folder). |
| `-h, --help`, `-v, --version` | |

```sh
fsm-generator "models/**/*.fsm" -t sml -o src/generated
```

- The machines are validated first. Nothing is generated when they have errors, and warnings are reported.
- Machines used by submachine states are read and validated too, but only the given files are generated. Generate the submachines' files as well, e.g. with `"models/*.fsm"`.
- Quote glob patterns (`*`, `?`, `[...]`, `**`): the program expands them itself, the same in bash, zsh, PowerShell and cmd.
- Problems, including the errors found while building the code model (such as pseudostates forming a cycle) and the warnings and errors a template reports, are printed to stderr as `file:line: severity: message`, which editors' problem matchers and CI logs understand.
- Exit codes: `0` success, `1` errors in the machines or the template, `2` bad usage.
- Generated files are always overwritten.

## Bundled templates

| Template | Writes, for a machine named `Order` |
| --- | --- |
| `ts` | TypeScript without dependencies: `OrderActions.ts` (events, states, the `OrderActions` interface to implement) and `Order.ts` (the `Order` class). |
| `sml` | C++17 for [Boost.SML](https://github.com/boost-ext/sml): `OrderEvents.h`, `OrderActions.h`, `Order.h`, `Order.cpp`, and `FsmTimers.h` shared by the machines with time events. |

Both implement UML run-to-completion semantics: an event queue, completion events, deferred events, timers through an injectable clock, do activities as start/stop hooks plus `activityDone()`, constraint checks reported to `onConstraintViolation`, one class per submachine, and `onTerminate` for terminate pseudostates. FSM Editor's [CODEGEN.md](https://github.com/vincedupuis/fsm-editor-vscode/blob/main/docs/CODEGEN.md) describes how to use the generated code and what the Boost.SML template works around.

## Writing a template

A template is a Jinja file, optionally starting with front matter between `---` lines:

```jinja
---
lang: cpp                   # preset for the guard and id filters: c, cpp, ts, java, python
guard:
  prefix: "actions_."       # overrides the preset: prefix, suffix, not, and, or, true
keywords: [signals, slots]  # more reserved words for the id filter
options:                    # free values, available as {{ options.namespace }}
  namespace: fsm
---
{% filter file(machine.name ~ "Actions.h") %}
#pragma once
namespace {{ options.namespace }} {
struct {{ machine.name }}Actions {
{% for a in actions %}
    virtual void {{ a | id }}() = 0;
{% endfor %}
{% for c in conditions %}
    virtual bool {{ c | id }}() = 0;
{% endfor %}
};
}
{% endfilter %}
```

The front matter is a small YAML subset: nested `key: value` maps, `- item` lists, inline `[a, b]` lists, quoted strings, numbers, booleans and `#` comments.

The template is rendered once per generated machine. Only the content of `file` blocks is written; text outside them is ignored. Machines may write the same file (a shared header) when they give it the same content. File names are relative to the output folder and may contain folders (`"include/" ~ machine.name ~ ".h"`), but not `..`.

Templates are data: they can't run code, and every filter is built in, so generating code from a repository never runs code from it.

### Variables

The [code model](https://github.com/vincedupuis/fsm-editor-vscode/blob/main/docs/CODEGEN.md#code-model-reference) of the machine, one variable per field: `machine`, `states`, `regions`, `topRegions`, `events`, `timers`, `signals`, `actions`, `conditions`, `activities`, `transitions`, `submachines`, `entryPoints`, `exitPoints`, `start`, `invariants`, `features` and `tables`. Besides them, `options` (the front matter's `options`), `config` (the whole front matter) and `lang` (the language settings: `name`, `keywords`, `escape`, `guard`).

### Filters and functions

Besides Jinja's own (`join`, `map`, `selectattr`, `length`, `first`, `indent`, `lower`, `tojson`, ...):

| | Example | Result |
| --- | --- | --- |
| `file` | `{% filter file("a/B.h") %}…{% endfilter %}` | Writes the block's output to a file. |
| `guard` | `{{ t.guard \| guard }}`, `{{ c \| guard(prefix="self.") }}` | Condition tree in the template's language; `true` (the preset's) when there is none. |
| `id` | `{{ "delete" \| id }}` | Escapes reserved words: `delete_`. |
| `pascal`, `camel`, `snake`, `upper_snake` | `{{ "blinkLed" \| upper_snake }}` | `BLINK_LED`, also escaping reserved words. |
| `quote` | `{{ label \| quote }}` | A double-quoted string literal (C, C++, Java, TypeScript, Python). |
| `warn()` | `{% do warn("No forks: ", name, id=id) %}` | Reports a warning about the element with xmi:id `id` (optional), at its line in the `.fsm` file. Arguments are joined. |
| `error()` | `{% do error("Cannot generate ", name, id=id) %}` | Reports an error: every error is printed once the template has been rendered, and nothing is written. |

### Whitespace

Templates are rendered with `trim_blocks` and `lstrip_blocks`, the trailing newline kept, and no HTML escaping:

- A block tag (`{% for %}`, `{% if %}`, `{% endfor %}`, ...) alone on its line leaves no line, and so does a comment.
- A block tag at the end of a line also removes that line's newline: write `{{ ", " if not loop.first }}` rather than ending a line with `{% endif %}`, or use `+%}`.
- `{%-`, `-%}`, `{{-` and `-}}` remove the white space (newlines included) before or after the tag: `= { {{- list | join(", ") -}} };` prints `= {1, 2};`.
- Booleans print as `True` and `False`: write `{{ s.isFinal | lower }}` for `true`. Fields that may be null (`parent`, `submachine`, `guard`, ...) print as `None`: test them before printing.

Multi-line pieces, such as the steps of a transition, are macros called with `indent`, which indents every line but the first (the call's own indentation). An excerpt, printing three kinds of steps:

```jinja
{% macro steps(list) %}
{% for s in list %}
{% if s.kind == "call" %}
actions.{{ s.action | id }}();
{% elif s.kind == "enter" %}
enter({{ s.stateIndex }}); // {{ s.state }}
{% elif s.kind == "choice" %}
{% for b in s.branches %}
{{ "if" if loop.first else "} else if" }} ({{ b.guard | guard }}) {
  {{ steps(b.steps) | indent(2) }}
{% endfor %}
}
{% endif %}
{% endfor %}
{% endmacro %}

void fire(int t) {
    switch (t) {
{% for t in transitions %}
    case {{ t.index }}:
{% if t.steps %}
        {{ steps(t.steps) | indent(8) }}
{% endif %}
        break;
{% endfor %}
    }
}
```

Test the list before such a call when it may be empty, or the call's indentation is left as a line of spaces. `templates/ts.jinja` is a complete example to copy: it handles every step kind and has the small runtime the steps rely on.

## Coming from the Handlebars templates

| Handlebars | Jinja |
| --- | --- |
| `{{#file (concat machine.name ".h")}}…{{/file}}` | `{% filter file(machine.name ~ ".h") %}…{% endfilter %}` |
| `{{guard guard prefix="self."}}`, `{{id name}}`, `{{pascal name}}` | `{{ guard \| guard(prefix="self.") }}`, `{{ name \| id }}`, `{{ name \| pascal }}` |
| `{{upper name}}` | `{{ name \| upper_snake }}` (Jinja's `upper` only changes the case) |
| `{{join states ", " "index"}}` | `{{ states \| map(attribute="index") \| join(", ") }}` |
| `{{quote label}}` | `{{ label \| quote }}` |
| `(includes list x)`, `(eq a b)`, `(and a b)`, `(not a)` | `x in list`, `a == b`, `a and b`, `not a` |
| `{{#switch kind}}{{#case "a" "b"}}…{{/case}}{{#default}}…{{/default}}{{/switch}}` | `{% if kind in ["a", "b"] %}…{% else %}…{% endif %}` |
| `{{warn "x" id=id}}`, `{{error "x" id=id}}` | `{% do warn("x", id=id) %}`, `{% do error("x", id=id) %}` |
| `{{#each list}}{{#unless @first}}, {{/unless}}…{{/each}}`, `@index`, `@last` | `{% for x in list %}{{ ", " if not loop.first }}…{% endfor %}`, `loop.index0`, `loop.last` |
| `{{../name}}`, `@root.machine` | name the loop variables (`{% for s in states %}`); top-level variables are visible everywhere |
| `{{#*inline "steps"}}…{{/inline}}` and `{{> steps}}` | `{% macro steps(list) %}…{% endmacro %}` and `{{ steps(list) \| indent(n) }}` |
| `{{lookup map key}}` | `map[key]` |
| `{{list.length}}` | `list \| length`, or `list` alone in a test |
