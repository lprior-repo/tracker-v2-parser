# Scott DDD + Types (Strict Mode, Compressed)

```jsonl
{"kind":"meta","doc":"scott-ddd-types","skill":"functional-rust","version":"5.0.0","compressed":true}
{"kind":"intent","text":"Strict default: type-driven DDD, zero-copy parsing, explicit workflows, compile-time correctness"}
{"kind":"position","points":["Domain correctness in types","Invalid states unconstructable","Workflows explicit and linear","Core consumes trusted, zero-copy types"]}
{"kind":"non_negotiable","points":["No primitives in domain APIs","No String allocations at parsing boundary","No bool control flags","No Option lifecycle","No deep nesting"]}
{"kind":"design_pattern","steps":["Parse boundary -> Zero-copy newtypes (&'a str, Cow, Bytes)","DTO->domain","Pure transitions in core","I/O in shell only"]}
{"kind":"transition_signature","ex":["Unvalidated<'a> -> Result<Validated<'a>, E>","Validated<'a> -> Result<Priced<'a>, E>"]}
{"kind":"transition_contract","must":["source","target","expected failures (NO unwrap)"]}
{"kind":"error_discipline","core":"thiserror","shell":"anyhow+context","ban":["Result<T,String> in core", "Swallowed errors (catch without propagate)"]}
```
