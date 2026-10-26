# Typing Refactor Checklist (Compressed)

```jsonl
{"kind":"meta","doc":"typing-refactor-checklist","skill":"functional-rust","version":"5.0.0","compressed":true}
{"kind":"check","group":"Holzmann Constraints","text":"Verify max 2 levels of nesting (linear control flow)"}
{"kind":"check","group":"Holzmann Constraints","text":"Verify NO imperative loops (for/while/loop)"}
{"kind":"check","group":"Holzmann Constraints","text":"Verify ZERO swallowed errors (all results handled)"}
{"kind":"check","group":"Types & DDD","text":"Identify primitives->domain concepts"}
{"kind":"check","group":"Types & DDD","text":"Add newtypes with fallible constructors"}
{"kind":"check","group":"Types & DDD","text":"Replace bool params with enums"}
{"kind":"check","group":"Types & DDD","text":"Replace status+Option with state enums"}
{"kind":"check","group":"Performance","text":"Replace owned Strings with &'a str, Cow<'a, str>, or bytes::Bytes at boundaries"}
{"kind":"check","group":"Performance","text":"Replace Vec with smallvec::SmallVec where sizes are small"}
{"kind":"check","group":"Performance","text":"Parallelize pure collection transforms with Rayon"}
{"kind":"check","group":"Safety","text":"thiserror for domain errors, ANYHOW at shell"}
{"kind":"check","group":"Safety","text":"Remove wildcard arms from domain matches"}
{"kind":"exit","text":"Compiler rejects illegal states & zero unnecessary allocations occur"}
```
