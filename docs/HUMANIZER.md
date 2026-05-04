# CRUX Humanizer

`crux humanize` rewrites raw AI-flavoured prose into concise,
human-sounding text. The rewrite is **deterministic and local** —
no LLM round-trip, no network — so the same input always yields
the same output and CI / golden tests can pin behaviour exactly.

## What it changes

- Drops AI-tell openers and closers: `In conclusion, …`,
  `It is important to note that …`, `As an AI language model, …`,
  `I hope this helps!`, `Let's dive in!`.
- Strips sycophantic openers: `Certainly!`, `Sure!`,
  `Great question!`, `You're absolutely right!`,
  `I'd be happy to help you with that`, `Of course`, `Absolutely`.
- Strips AI self-reference: `I'll walk you through` →
  `walk through` (plus the uppercased sentence-start rewrite).
- Drops fillers (`basically`, `just`, `really`, `very`,
  `actually`, `simply`, `clearly`) in every mode except
  `professional`.
- Collapses wordy phrases: `in order to` → `to`,
  `due to the fact that` → `because`, `at this point in time` →
  `now`, `the majority of` → `most`.
- Replaces fluffy verbs: `utilize` → `use`, `leverage` → `use`,
  `facilitate` → `help`, `commence` → `start`, `delve` → `dig`,
  `endeavor` → `try`. Case is preserved.
- Removes marketing adjectives in every mode except
  `professional`: `robust`, `comprehensive`, `seamless`,
  `cutting-edge`, `state-of-the-art`, `groundbreaking`, …
- Collapses adjacent repeated words (`very very` → `very`).
- Tidies whitespace, excessive blank lines, and strips orphan
  leading punctuation left behind by a pleasantry removal
  (`! In this article, …` → `In this article, …`).

## What it never touches

- Fenced code blocks and inline code spans.
- URLs (`http://`, `https://`, `www.example.com`).
- Filesystem paths (`/foo/bar`, `./foo`, `C:\foo`).
- Hex / IPv4 / IPv6-like literals (`0xdeadbeef`, `127.0.0.1`,
  `fe80::1`).
- Identifier-shaped tokens (`foo::bar::baz`, `foo(args)`,
  `@scope/pkg`).
- `SCREAMING_SNAKE_CASE` constants.

## Modes

| Mode | What it tunes |
|---|---|
| `concise` | Aggressive trim. Strips every buzzword + fluff adjective + filler word. Default. |
| `casual` | Concise + contractions (`it is` → `it's`, `do not` → `don't`). |
| `professional` | Strips pleasantries but keeps formal connectors, adjectives, and fillers. |
| `developer` | Terse and technical. No pleasantries, no fluff, no contractions. |
| `social` | Short sentences + contractions. Good for Twitter / Mastodon. |
| `github-readme` | README-friendly: keeps blank lines and headings, strips filler. |

## Examples

```bash
# Inline rewrite — pleasantries + fillers stripped, output capitalised
crux humanize --mode concise \
  --input "I'd be happy to help you with that! In this article, I'll walk you through how to basically just really simplify your code."
# → Walk through how to simplify your code.

# Whole-file rewrite
crux humanize --mode developer --file output.md > clean.md

# Pipe stdin
cat answer.txt | crux humanize --mode social

# JSON output (text + before/after stats)
crux humanize --mode casual --input "It is great." --json
# { "mode": "casual", "text": "It's great.", "stats": { ... } }

# Stderr stats footer (does not pollute stdout for piping)
crux humanize --mode concise --file output.md --stats
```

## Extending

The rule tables live in `crates/crux-humanizer/src/rules.rs` —
adding a new strike phrase, word substitution, pleasantry opener,
or filler is a one-line change with a co-located test.
