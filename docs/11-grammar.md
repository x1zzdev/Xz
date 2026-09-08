# Grammar — Full Specification

This is the **authoritative grammar** for the Xz language surface. It fixes
every construct that may appear in source files and how they combine, so the
front end (Phase 1) can be implemented against a stable target.
[02-syntax.md](02-syntax.md) is the readable overview; when the two disagree,
**this document wins**.

## Layout and block structure

- **Blocks are delimited by braces** `{ }`. Indentation is a **mandatory
  layout convention** (4 spaces, no tabs), enforced by the formatter, not by
  the parser.
- Statements end at a **newline**; there are no semicolons. A `;` is a syntax
  error. Long expressions are continued with parentheses.
- A block's value is the value of its **final expression**; a function body
  is a block, so the last expression is the return value.

> Why braces, despite "indentation-based"? Every block in the corpus and every
> example already uses braces; "no braces" was a contradiction with the code
> the design produced. Explicit delimiters also remove the ambiguity an
> indentation-only parser leaves for generated code — a real concern for the
> AI-writing target. Indentation remains, as a uniform layout the formatter
> guarantees.

## Lexical grammar

```
line_comment   := "//" (any char except newline)*
block_comment  := "/*" any* "*/"                       // no nesting

ident          := [A-Za-z_][A-Za-z0-9_]*

int_literal    := digit (["_"] digit)*                 // 42, 1_000_000
               | "0x" hex_digit (["_"] hex_digit)*     // 0x7f, 0xff_ff
float_literal  := digit+ "." digit+ (["e"|"E"] ["+"|"-"]? digit+)?
               | digit+ (["e"|"E"] ["+"|"-"]? digit+)  // 1e10, 1.5e-3
char_literal   := "'" (escape | any except "'" "\" newline) "'"
str_literal    := "\"" (escape | any except "\"" "\" newline)* "\""
raw_str        := "r\"" any* "\""                       // no escapes
escape         := "\\" ("n" | "t" | "r" | "\\" | "\"" | "'")
               | "\\u{" hex_digit+ "}"                  // Unicode scalar
```

**Keywords** (reserved; cannot be identifiers):

```
and  as  async  await  break  chan  continue  elif  else  enum  err
extern  false  for  func  if  implies  in  invariant  is  let  loop
match  mut  none  not  ok  or  post  pre  recv  record  send  some
task  transfer  true
```

`true` / `false` / `none` are keywords and literal tokens.

**Punctuation and operators:**

```
( ) [ ] { }  ,  :  .  ->  <-  ?  +  -  *  /  %  ==  !=  <  <=  >  >=
=  +=  -=  *=  /=  |  _
```

**Naming conventions** (enforced by the linter, not the parser):

- Values and functions: `lowerCamelCase`
- Types (records, enums, generic names): `PascalCase`
- Immutable constants: `SCREAMING_CASE` — the only sanctioned exception
  (`PI`, `E`; see [12-stdlib.md](12-stdlib.md))
- `_` is the wildcard, valid only in match patterns

## Operator precedence

Lowest to highest; `a op b op c` chains left-associatively except where noted.

| Level | Operators | Notes |
|---|---|---|
| 1 | `implies` | contract expressions only, right-associative |
| 2 | `or` | |
| 3 | `and` | |
| 4 | `not` | prefix |
| 5 | `as` | cast: `expr as Type` |
| 6 | `==` `!=` `<` `<=` `>` `>=` | compare |
| 6 | `is` | `expr is ok` / `is err` / `is none` / `is some` — narrows the subject |
| 7 | `+` `-` | additive; `+` on `Str` is concatenation |
| 8 | `*` `/` `%` | multiplicative |
| 9 | `-` | unary minus, prefix |
| 10 | postfix | `call(args)` `.field` `[index]` `?` |
| 11 | primary | literals, `IDENT`, `(...)`, match/if/loop/for, constructors |

`?` binds tighter than `as`: `a()? as Str` parses as `(a()?) as Str`.
`await` applies to the immediately following postfix chain:
`await fetch(url)?` parses as `(await fetch(url))?`.

## Syntactic grammar

```
program         := top_level*
top_level       := func_decl | task_decl | chan_decl | extern_decl
                 | record_decl | enum_decl

func_decl       := "async"? "func" IDENT "(" params ")" ("->" type)? contract* block
task_decl       := "task" IDENT block
chan_decl       := "chan" IDENT ":" "Chan[" type "]"
extern_decl     := "extern" "func" IDENT "(" params ")" ("->" type)?
record_decl     := "record" IDENT "{" field* "}"
enum_decl       := "enum" IDENT "{" variant+ "}"

params          := param ("," param)*
param           := ("mut")? IDENT ":" type
field           := IDENT ":" type
variant         := IDENT "(" (field ("," field)*)? ")"

contract        := "pre" expr
                 | "post" expr
                 | "invariant" expr

block           := "{" statement* "}"
statement       := decl | assignment | expr | "break" | "continue"
decl            := ("let" | "mut") IDENT ":" type ("=" expr)?
                 | ("let" | "mut") IDENT "<-" "recv" "(" expr ")"
assignment      := (IDENT | expr "." IDENT) assign_op expr
assign_op       := "=" | "+=" | "-=" | "*=" | "/="
```

```
expr            := contract_expr
contract_expr   := or_expr ("implies" contract_expr)?      // right-assoc
or_expr         := and_expr ("or" and_expr)*
and_expr        := not_expr ("and" not_expr)*
not_expr        := "not" not_expr | cast_expr
cast_expr       := comparison ("as" type)?
comparison      := additive (comp_op additive)*
comp_op         := "==" | "!=" | "<" | "<=" | ">" | ">="
                 | "is" ("ok" | "err" | "none" | "some")
additive        := multiplicative (("+" | "-") multiplicative)*
multiplicative  := unary (("*" | "/" | "%") unary)*
unary           := "-" unary | postfix
postfix         := primary post_op*
post_op         := "(" args ")" | "." IDENT | "[" expr "]" | "?"
args            := expr ("," expr)*
primary         := literal | IDENT | "(" expr ")"
                 | "match" expr "{" match_arm+ "}"
                 | "if" expr block ("elif" expr block)* ("else" block)?
                 | "loop" block
                 | "for" IDENT "in" expr block
                 | "await" postfix
                 | "send" "(" expr "," expr ")"
                 | "transfer" "(" expr ")"
                 | "ok" "(" (expr)? ")" | "err" "(" expr ")"
                 | "some" "(" expr ")" | "none"
match_arm       := pattern "->" expr
pattern         := "_" | "none"
                 | "ok" "(" (IDENT ("," IDENT)*)? ")"
                 | "err" "(" (IDENT ("," IDENT)*)? ")"
                 | "some" "(" (IDENT ("," IDENT)*)? ")"
                 | IDENT | IDENT "(" (IDENT ("," IDENT)*)? ")"

type            := union_type
union_type      := nominal ("|" nominal)*
nominal         := prim | IDENT ("[" type ("," type)* "]")?
prim            := "Bool" | "Int" | "usize" | "Float" | "Char" | "Str" | "Bytes"
                 | "Unit" | "Ptr"
                 | "Option[" type "]" | "Result[" type "," type "]"
                 | "Chan[" type "]"
```

## Intent comments

Public `func`/`task` declarations (except `main`) require an intent comment
immediately above them (see [09-intent-verification.md](09-intent-verification.md)):

```
intent_block    := intent_line+
intent_line     := "///" "intent"  NL_TEXT
                 | "///" "@requires" NL_TEXT trusted?
                 | "///" "@ensures"  NL_TEXT trusted?
                 | "///" "@effects"  effect_list
trusted         := "@trusted" "//" "reviewed by" IDENT "on" DATE
effect_list     := "none" | ("mut" | "io" | "chan" | "extern") ("," effect_list)?
```

## Well-formedness constraints

The grammar alone is not the whole contract. The compiler also enforces:

- **Contract-point explicitness** — parameter, return, record-field, and
  channel-payload types are mandatory ([03-type-system.md](03-type-system.md)).
- **`?` acceptance** — `expr?` is legal only if the caller's declared error
  channel accepts the callee's ([06-error-handling.md](06-error-handling.md)).
- **Match exhaustiveness** — every `match` must cover all variants; no
  fall-through, no default (add `_` to ignore).
- **Handle affinity** — `Ptr`-bearing records are never copied; handoff is
  `transfer(x)` only ([10-ffi-interop.md](10-ffi-interop.md)).
- **Effect honesty** — the derived effect profile must equal `@effects`
  ([09-intent-verification.md](09-intent-verification.md)).
- **Claim pairing** — `@requires`/`@ensures` pair (in order) with `pre`/`post`.

## What is deliberately not specified here

- Name resolution, scoping, and shadowing rules — Phase 1 implementation
  detail, constrained by the philosophy's explicitness principle.
- Operator overloading — there is none. `+` on `Str` is the only built-in
  case of a symbol meaning more than one thing, and it is fixed by the
  language.
- Collections `[]` indexing — reserved syntax; no stdlib type defines it
  until [12-stdlib.md](12-stdlib.md) grows collections (Phase 7).