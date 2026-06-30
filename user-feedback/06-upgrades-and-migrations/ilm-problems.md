# Index Lifecycle Management (ILM) Problems

## Severity: MODERATE | Frequency: MODERATE

---

## Core Complaints

### Rollover Failures
- Index names must match regex `^.*-\d+` for rollover to work
- Non-conforming names silently fail
- Only one index can be the write index for an alias
- Configuration errors are common, error messages unclear

### min_age Confusion
- When using rollover, min_age is relative to rollover time, not index creation time
- Causes unexpected behavior: indices don't transition when expected
- Counter-intuitive for operators who think in terms of "data age"

### Policy Doesn't Carry Forward
- Manually applied ILM policies don't carry to new indices from rollover
- ILM configured after index creation uses original settings, not updated ones
- Forum example: daily rollover configured but ILM didn't trigger for 2 weeks (old 30-day policy still active)

### Error Halts Execution
- When ILM encounters an error, execution HALTS at ERROR step
- Requires manual intervention to resume
- No automatic recovery for most error conditions

### Security Interaction
- ILM executes as the user who last updated the policy
- Uses that user's roles AT TIME OF LAST UPDATE
- Role changes or user deletion silently break ILM operations

### Late-Arriving Data Rejection
- In time-series data streams, documents with timestamps outside writable backing index range are rejected
- Must keep backing indices writable long enough for delayed data (6+ hours for late metrics)

### Shard Shrink Impossibilities
- ILM shrink action creates node-ID routing constraints
- Shrink operations can be mathematically impossible (e.g., 5 shards to 2)
- Failed shrinks leave shards permanently unassigned

---

## Forum Examples

> "ILM does not trigger rollover" -- user waited 2 weeks for daily rollover that never happened
> -- Elastic Forum #348711

> "Shard allocation failed -- even manually" -- 256 replica shards permanently unassigned after hitting 10K shard limit
> -- Elastic Forum #245357

---

## XERJ.ai Response
- No ILM needed in M1 (simpler retention model)
- TTL-based retention: configure `retention_days`, background purge handles it
- Time-partitioned segments: old data = old segments, just delete them
- No shard shrink operations (no shards)
- No alias-based rollover complexity

## Sources
- Elastic Docs: ILM Errors
- Elastic Forum: #348711, #245357
- Elastic Docs: ILM
