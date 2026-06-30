# Battle Test: drain-outside-lock fix
Date: Thu Apr 16 05:24:40 PM UTC 2026
Binary: target/release/xerj: ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), dynamically linked, interpreter /lib64/ld-linux-x86-64.so.2, for GNU/Linux 3.2.0, BuildID[sha1]=4141e9fbc9b8c5faadbaf7d7fcbcde006cd6572c, stripped

## Fix Applied
- drain_shard: release write lock BEFORE simd-json parse (was parsing 100k+ docs under lock)
- .cargo/config.toml restored (target-cpu=native)
- peek_shard_has_raw_bytes: skip FTS build for raw-bytes path

## 20M Doc Ingest (3 runs)

### Run 1
```
unknown argument: --insecure. Use --help for usage.
```

### Run 2
```
unknown argument: --insecure. Use --help for usage.
```

### Run 3
```
unknown argument: --insecure. Use --help for usage.
```

## Comparison
| Baseline | Pre-simd-json | Post-simd-json (regression) | This fix |
|---|---|---|---|
| ES 86k/s | ~880-950k/s | ~515-626k/s | see above |
