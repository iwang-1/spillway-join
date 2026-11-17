# Spillway Join

[![Rust](https://img.shields.io/badge/rust-stable-B7410E?logo=rust&logoColor=white)](https://www.rust-lang.org/) [![dependencies: none](https://img.shields.io/badge/dependencies-none-brightgreen)](Cargo.toml)

A Rust external-memory inner equijoin for two TSV files. Each input row contains a signed 64-bit key and payload. The output contains one triple for **every** matching pair, including duplicates. You choose how many build-side rows the operator may hold in a block.

## At a glance

- **Exact join under a fixed memory budget.** Matches SQLite **row for row, including duplicate counts**, on 32 seeded cases and 100,000-row fixtures.
- **2.6x faster after profiling:** choosing partition fanout adaptively cut median end-to-end time from **1.227 s to 0.468 s** on the 100k-row uniform fixture.
- **Skew-proof.** A hot-key join produced **689,228** triples with only 256 rows in memory; count mode verified **1,200,000,323** matching pairs without writing them.
- Hash partitioning with recursive re-seeding, block nested-loop fallback, atomic no-clobber output publishing.

## Run a join

```sh
cargo build --release --locked
printf '7\t10\n7\t11\n' > left.tsv
printf '7\t90\n8\t12\n' > right.tsv
target/release/spillway-join left.tsv right.tsv joined.tsv 1
cat joined.tsv
```

This emits both matches for key 7. The destination must be a new file. Output order depends on partitioning. Standard output reports row counts, spill writes, probe reads and peak resident build rows as JSON.

## How the budget changes execution

1. Validate and spool each input to fixed-width little-endian records. Oversized lines fail with their source line number.
2. If the smaller relation fits, load it into a sorted block. Stream the larger relation and find each matching range by binary search. Preserve logical left/right payload order even when the physical build side changes.
3. Otherwise hash-partition both sides with the same key mix. Pick a fanout based on build size and budget, capped at 16. Process matching partitions recursively with a new mix at each level.
4. A heavy key stays together regardless of the hash seed. If the smaller child does not shrink, or three partition levels have been reached, read the smaller side in bounded blocks and rescan the larger side once per block. This terminates with the exact output multiplicity even on an all-equal key stream.

The build budget counts 16-byte records. Process memory also includes sorting state, I/O buffers, partition metadata and the runtime. A reduced build budget can increase execution time because additional partitions create filesystem work. An all-equal join can have quadratic output size regardless of the build budget.

Temporary partitions live beside the destination. The operator flushes the completed output before publishing it by hard link, so a concurrent writer cannot overwrite an existing destination. Normal failures clean up scratch files. Abrupt termination can leave a scratch directory. The filesystem must support hard links. There is no power-loss durability or resume guarantee.

## Reproduce the evidence

```sh
cargo build --release --locked
python3 -m unittest discover -v
python3 experiments/verify.py
python3 experiments/measure.py
```

The verifier feeds the same two relations to SQLite and Spillway, sorts both outputs in SQLite and compares every triple including duplicate counts. The suite covers 32 seeded small cases, empty relations, extreme signed integers, a hot-key fallback, malformed input and four simultaneous output creators. The larger experiment uses 100,000 rows per input, two budgets and two distributions. See the [operator counters](experiments/results.json).

Timing uses three fresh child processes per configuration, rotating their execution order. Each implementation reads the same TSV files and emits TSV. The reference uses Python's SQLite binding for parsing, insertion and output formatting, so its timing includes interpreter overhead. Both use the host filesystem cache; the experiment does not flush it between trials. OS-reported peak RSS includes runtime overhead. Timings are host-specific and the fixtures are synthetic. [Full measurements](experiments/measurements.json)

## What changed after measuring the first version?

The first implementation opened 16 bucket files per input for every split, even near the leaves. On the uniform fixture with a 256-row build budget, its median end-to-end time was **1.227 seconds**. Choosing the fanout from the estimated number of blocks reduced that median to **0.468 seconds** across three trials per version on the same host. The run still wrote 9.6 MB of partition records. The improvement came with fewer bucket files and fewer probe scans; the experiment does not isolate their individual contributions. The Python/SQLite pipeline took **0.243 seconds** on that fixture. [Fixed-fanout measurements](experiments/fixed-fanout-measurements.json) · [Adaptive-fanout measurements](experiments/measurements.json)

On the hot-key fixture, the 256-row build budget triggered one skew fallback and returned **689,228 triples**, matching SQLite row for row. Increasing the budget to 4,096 rows avoided the fallback and reduced partition writes from 9.6 MB to 6.4 MB. Those counters exclude initial spooling and result output.

## Count a join whose output is too large to store

Use `target/release/spillway-join --count left.tsv right.tsv 512` to compute inner-join cardinality. Count mode uses the same partition and skew paths but accumulates the width of matching sorted ranges using checked 64-bit arithmetic. No result triples are serialized. Temporary partitions live in the current directory. The regression constructs two hot keys and verifies **1,200,000,323 matching pairs** against the product of key frequencies while using at most 512 build rows. This figure measures logical cardinality; the experiment does not write a billion rows.
