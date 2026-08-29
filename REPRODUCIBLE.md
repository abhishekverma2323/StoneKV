# REPRODUCIBLE.md

**Bonus claim — Reproducible Build.** Two independent, fully clean
release builds of Stone on the same machine produce byte-identical
binaries, verified by SHA-256.

## How to reproduce this

```bash
bash scripts/build_repro.sh
```

The script runs `cargo clean` followed by `cargo build --release`
twice in a row, hashing `target/release/stone` (or `stone.exe` on
Windows) after each build, and reports `PASS` only if both hashes
match exactly.

## Verified result

Run on:

```
rustc 1.97.1 (8bab26f4f 2026-07-14)
cargo 1.97.1 (c980f4866 2026-06-30)
Linux Abhishek 6.18.33.2-microsoft-standard-WSL2 #1 SMP PREEMPT_DYNAMIC
Thu Jun 18 21:54:43 UTC 2026 x86_64 GNU/Linux
```

```
=== First clean release build ===
first hash:  a7be952baf90dcda665df20f7e8a950530210e01dfd10dcd37a1a556b6c3edce

=== Second clean release build ===
second hash: a7be952baf90dcda665df20f7e8a950530210e01dfd10dcd37a1a556b6c3edce

PASS: release binaries are identical in this environment.
```

Both hashes are identical: `a7be952baf90dcda665df20f7e8a950530210e01dfd10dcd37a1a556b6c3edce`.

## Why this is possible with zero dependencies

Stone has no dependency graph to resolve, so there is no version drift,
no build-script code generation from a third-party crate, and no
network fetch step that could vary between builds. The build's only
inputs are the Rust toolchain itself and this repository's own source
— both fixed and local — so a clean rebuild has nothing external left
to vary.

## Scope of this claim

This result is reproducible **on a single machine, back-to-back, with
a fixed toolchain version.** It has not been verified across different
operating systems, architectures, or Rust compiler versions, and Rust
release binaries are not guaranteed reproducible across compiler
versions in general (codegen and optimizer internals can change
between releases). The claim here is specifically: *this build has no
source of non-determinism introduced by dependencies*, which is the
zero-dependency-relevant property being demonstrated.
