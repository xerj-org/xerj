# Real CLI / API outputs captured on this machine

Date:     2026-04-27T19:11:24Z
Host:     drai · Linux 7.0.0-14-generic · 32 cores · 119Gi RAM
Binary:   22227912 bytes (22M)
Version:  xerj v1.0.0-rc.1
Commit:   78a9bccd8d37906e4cee0c4f772ac8a3053e1c66
Tag:      v1.0.0-rc.1

## 1. CLI ingest — 655 K real loghub OpenSSH lines

```
$ ./xerj index --index ssh-auth --file demo-data/ssh_one.ndjson \
      --workers 8 --batch 5000 --data-dir /home/claude/ai/xerj/demo/.cli-validate
[2m2026-04-27T19:11:33.345273Z[0m [32m INFO[0m [2mxerj[0m[2m:[0m no config file — using defaults
[2m2026-04-27T19:11:33.345329Z[0m [33m WARN[0m [2mxerj[0m[2m:[0m --insecure: TLS and auth disabled
[2m2026-04-27T19:11:33.345338Z[0m [32m INFO[0m [2mxerj[0m[2m:[0m xerj CLI index: starting [3mindex[0m[2m=[0m"ssh-auth" [3mfile[0m[2m=[0mdemo-data/ssh_one.ndjson [3mbatch[0m[2m=[0m5000 [3mworkers[0m[2m=[0m8
[2m2026-04-27T19:11:33.358117Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358143Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358154Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358165Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358175Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358186Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358196Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358206Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358216Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358227Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358237Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358247Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358257Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358267Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358277Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358286Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m no checkpoint found, replaying from generation 0
[2m2026-04-27T19:11:33.358291Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m IndexStore opened [3mdata_dir[0m[2m=[0m"/home/claude/ai/xerj/demo/.cli-validate/ssh-auth"
[2m2026-04-27T19:11:33.358401Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m index created [3mname[0m[2m=[0m"ssh-auth"
[2m2026-04-27T19:11:33.358453Z[0m [32m INFO[0m [2mxerj_engine::engine[0m[2m:[0m index created [3mname[0m[2m=[0m"ssh-auth"
[2m2026-04-27T19:11:33.358470Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m merge background task started [3minterval_secs[0m[2m=[0m5
[2m2026-04-27T19:11:33.669376Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.670530Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"e04c6e06-7866-4343-97fa-dc57a74c2e77" [3mdoc_count[0m[2m=[0m12096 [3mmin_seq[0m[2m=[0m390001 [3mmax_seq[0m[2m=[0m645525
[2m2026-04-27T19:11:33.672378Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"e04c6e06-7866-4343-97fa-dc57a74c2e77" [3mdoc_count[0m[2m=[0m12096
[2m2026-04-27T19:11:33.675348Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.679506Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.683048Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.686295Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.689559Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.694676Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.699145Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.702718Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.705939Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.707909Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"91187a91-239b-4acb-ae62-46a40583618e" [3mdoc_count[0m[2m=[0m11750 [3mmin_seq[0m[2m=[0m455001 [3mmax_seq[0m[2m=[0m647275
[2m2026-04-27T19:11:33.709425Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.709453Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"91187a91-239b-4acb-ae62-46a40583618e" [3mdoc_count[0m[2m=[0m11750
[2m2026-04-27T19:11:33.714842Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.719579Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.722894Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"95c1943a-16ff-4c54-a31f-ae5277c3d64f" [3mdoc_count[0m[2m=[0m13488 [3mmin_seq[0m[2m=[0m555001 [3mmax_seq[0m[2m=[0m650763
[2m2026-04-27T19:11:33.724688Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.724715Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"95c1943a-16ff-4c54-a31f-ae5277c3d64f" [3mdoc_count[0m[2m=[0m13488
[2m2026-04-27T19:11:33.730303Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.735263Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:33.735436Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"5dbfc095-fa24-40e0-8641-f944020c1ac4" [3mdoc_count[0m[2m=[0m6807 [3mmin_seq[0m[2m=[0m530001 [3mmax_seq[0m[2m=[0m655147
[2m2026-04-27T19:11:33.736416Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"5dbfc095-fa24-40e0-8641-f944020c1ac4" [3mdoc_count[0m[2m=[0m6807
[2m2026-04-27T19:11:34.339821Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"798c51b1-1f59-4f01-a235-953c042514ee" [3mdoc_count[0m[2m=[0m27598 [3mmin_seq[0m[2m=[0m475001 [3mmax_seq[0m[2m=[0m638429
[2m2026-04-27T19:11:34.342421Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"798c51b1-1f59-4f01-a235-953c042514ee" [3mdoc_count[0m[2m=[0m27598
[2m2026-04-27T19:11:34.356371Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"38abecb3-f477-4d21-974e-171380a2750e" [3mdoc_count[0m[2m=[0m32131 [3mmin_seq[0m[2m=[0m1 [3mmax_seq[0m[2m=[0m625831
[2m2026-04-27T19:11:34.358199Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"0b4bf40a-d08d-4786-967d-da8046fbd563" [3mdoc_count[0m[2m=[0m30000 [3mmin_seq[0m[2m=[0m90001 [3mmax_seq[0m[2m=[0m643429
[2m2026-04-27T19:11:34.359312Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"38abecb3-f477-4d21-974e-171380a2750e" [3mdoc_count[0m[2m=[0m32131
[2m2026-04-27T19:11:34.360718Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"0b4bf40a-d08d-4786-967d-da8046fbd563" [3mdoc_count[0m[2m=[0m30000
[2m2026-04-27T19:11:34.361888Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"b0f7bfb9-fa2a-4e0f-8d1f-f1cc79477f9b" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m75001 [3mmax_seq[0m[2m=[0m365000
[2m2026-04-27T19:11:34.364945Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"b0f7bfb9-fa2a-4e0f-8d1f-f1cc79477f9b" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.371407Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"1828be49-5e4f-441d-acac-189b0290c771" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m70001 [3mmax_seq[0m[2m=[0m475000
[2m2026-04-27T19:11:34.374620Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"1828be49-5e4f-441d-acac-189b0290c771" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.472977Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"c6337bad-3106-4506-af93-9a259b9a6f42" [3mdoc_count[0m[2m=[0m25000 [3mmin_seq[0m[2m=[0m25001 [3mmax_seq[0m[2m=[0m590000
[2m2026-04-27T19:11:34.475799Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"c6337bad-3106-4506-af93-9a259b9a6f42" [3mdoc_count[0m[2m=[0m25000
[2m2026-04-27T19:11:34.477653Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"3a01dfa4-52c1-465d-85f4-a372133c749d" [3mdoc_count[0m[2m=[0m22577 [3mmin_seq[0m[2m=[0m375001 [3mmax_seq[0m[2m=[0m653340
[2m2026-04-27T19:11:34.479794Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"3a01dfa4-52c1-465d-85f4-a372133c749d" [3mdoc_count[0m[2m=[0m22577
[2m2026-04-27T19:11:34.583588Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"472fc5a9-4b40-4279-8d49-ec45c8594bd8" [3mdoc_count[0m[2m=[0m25000 [3mmin_seq[0m[2m=[0m10001 [3mmax_seq[0m[2m=[0m500000
[2m2026-04-27T19:11:34.586186Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"472fc5a9-4b40-4279-8d49-ec45c8594bd8" [3mdoc_count[0m[2m=[0m25000
[2m2026-04-27T19:11:34.772312Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.773376Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.774294Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.775150Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.775977Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.776683Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.777571Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.778420Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.779317Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.780216Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.781154Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.782046Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.782978Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.783831Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.784723Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.785616Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m623700
[2m2026-04-27T19:11:34.785759Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"44197da2-c58d-4393-a26b-dd1db967b0d4" [3mdoc_count[0m[2m=[0m30000 [3mmin_seq[0m[2m=[0m45001 [3mmax_seq[0m[2m=[0m623700
[2m2026-04-27T19:11:34.788569Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"44197da2-c58d-4393-a26b-dd1db967b0d4" [3mdoc_count[0m[2m=[0m30000
[2m2026-04-27T19:11:34.878140Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"50dbd1bb-327a-4836-9b5d-d6ee21b0b8ad" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m65001 [3mmax_seq[0m[2m=[0m445000
[2m2026-04-27T19:11:34.881003Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"50dbd1bb-327a-4836-9b5d-d6ee21b0b8ad" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.883895Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"9f6cfcd9-d8c4-4157-a4bb-6611d83a84e7" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m335001 [3mmax_seq[0m[2m=[0m565000
[2m2026-04-27T19:11:34.884683Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"1ff84a49-6021-484d-acfd-758fc1c2a355" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m160001 [3mmax_seq[0m[2m=[0m505000
[2m2026-04-27T19:11:34.888430Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"1ff84a49-6021-484d-acfd-758fc1c2a355" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.889022Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"9f6cfcd9-d8c4-4157-a4bb-6611d83a84e7" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.916568Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"70a8a04b-5cd5-46fe-91be-5a1a6e305528" [3mdoc_count[0m[2m=[0m25000 [3mmin_seq[0m[2m=[0m235001 [3mmax_seq[0m[2m=[0m575000
[2m2026-04-27T19:11:34.917913Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"b091f74f-1763-47b3-b39f-c6033cfb20ac" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m40001 [3mmax_seq[0m[2m=[0m390000
[2m2026-04-27T19:11:34.920590Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"70a8a04b-5cd5-46fe-91be-5a1a6e305528" [3mdoc_count[0m[2m=[0m25000
[2m2026-04-27T19:11:34.921423Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"b091f74f-1763-47b3-b39f-c6033cfb20ac" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.922039Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.923108Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.924543Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.925048Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"bf6fbd66-d547-4d1e-8f21-67b15d7ff176" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m60001 [3mmax_seq[0m[2m=[0m370000
[2m2026-04-27T19:11:34.925499Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.925870Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"d0f8d86e-eff6-4d89-9e39-83e4874e5e26" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m5001 [3mmax_seq[0m[2m=[0m635831
[2m2026-04-27T19:11:34.926928Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.927849Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.929159Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.930048Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"bf6fbd66-d547-4d1e-8f21-67b15d7ff176" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.930114Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.930278Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"d0f8d86e-eff6-4d89-9e39-83e4874e5e26" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:34.931231Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.932158Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.933279Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.934254Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.935413Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.936347Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.937860Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.938824Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.940168Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.941113Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.942381Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.943359Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.944475Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.945409Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.946583Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.947580Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.948784Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.949820Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.951061Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.952102Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.953426Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.954410Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1
[2m2026-04-27T19:11:34.955830Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m0 [3mmax_seq_no[0m[2m=[0m655147
[2m2026-04-27T19:11:34.956736Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL rotated to new generation [3mgeneration[0m[2m=[0m1

═══════════════════════════════════════════════════════════
 xerj index: complete
═══════════════════════════════════════════════════════════
 index          : ssh-auth
 file           : demo-data/ssh_one.ndjson
 file size      : 132 MB
 docs sent      : 655147
 errors         : 0
 ingest time    : 0.11 s
 ingest rate    : 6008939 docs/s  (WAL-durable, in-memtable)
 final flush    : 1.49 s
 total elapsed  : 1.60 s
 total rate     : 409809 docs/s  (fully segment-durable)
 workers        : 8
 batch size     : 5000
═══════════════════════════════════════════════════════════
[2m2026-04-27T19:11:35.021489Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"a3993acb-2523-43f6-9d44-cc21513b32ca" [3mdoc_count[0m[2m=[0m43700 [3mmin_seq[0m[2m=[0m30001 [3mmax_seq[0m[2m=[0m613700
[2m2026-04-27T19:11:35.024939Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"a3993acb-2523-43f6-9d44-cc21513b32ca" [3mdoc_count[0m[2m=[0m43700
[2m2026-04-27T19:11:35.131130Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"0afdab60-df32-480f-9450-92088acf89c2" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m115001 [3mmax_seq[0m[2m=[0m285000
[2m2026-04-27T19:11:35.134580Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"0afdab60-df32-480f-9450-92088acf89c2" [3mdoc_count[0m[2m=[0m35000
[2m2026-04-27T19:11:35.156347Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"346cf76c-2aab-4bba-bf80-1f5dac6d4b6f" [3mdoc_count[0m[2m=[0m35000 [3mmin_seq[0m[2m=[0m15001 [3mmax_seq[0m[2m=[0m290000
[2m2026-04-27T19:11:35.159382Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"346cf76c-2aab-4bba-bf80-1f5dac6d4b6f" [3mdoc_count[0m[2m=[0m35000
```

## 2. Boot — replays WAL from the ssh-auth ingest above

Time-to-ready (first `/_cluster/health` 200): **351 ms** (poll resolution 50 ms, so this is an upper bound).

Boot stderr (first 6 lines, ANSI colour codes stripped):
```

┌──────────────────────────────────────────────────────────────────────────────┐
│ XERJ CONSOLE  ·  first-launch setup                                                │
│                                                                              │
│ Open this link in your browser to claim the owner account by                 │
│ enrolling a passkey.  Valid for 30 minutes.  Single use.                     │
```

## 3. Cluster health · cat indices

```
$ curl -s localhost:9200/_cluster/health | jq .
{
  "active_primary_shards": 15,
  "active_shards": 15,
  "active_shards_percent_as_number": 100.0,
  "cluster_name": "xerj",
  "delayed_unassigned_shards": 0,
  "initializing_shards": 0,
  "number_of_data_nodes": 1,
  "number_of_in_flight_fetch": 0,
  "number_of_nodes": 1,
  "number_of_pending_tasks": 0,
  "relocating_shards": 0,
  "status": "green",
  "task_max_waiting_in_queue_millis": 0,
  "timed_out": false,
  "unassigned_primary_shards": 0,
  "unassigned_shards": 0
}

$ curl -s "localhost:9200/_cat/indices?v"
green open .xerj_cluster_state 9904db1c-4d82-4c14-bebc-a29ec3eded35 1 0 0 0
green open .xerj_connections 2feb99fa-b0be-4990-b37a-be4a81678476 1 0 0 0
green open .xerj_sessions 48ae7e12-2c22-4ff6-a2a1-d2c2f5651928 1 0 0 0
green open .xerj_prefs 8ea77c72-9dc8-435a-afd8-f0eaf6802919 1 0 0 0
green open .xerj_api_tokens 502834d4-76b1-4bec-adf9-4510dec8232f 1 0 0 0
green open .xerj_dashboards 3a11a61c-94d5-4749-9d48-3e7e2c0989e2 1 0 0 0
green open .xerj_views 2e19affb-df4a-4376-b933-0e24bb066676 1 0 0 0
green open .xerj_magic_links 9da661a1-fb90-4238-a2bd-3f01414c9bd8 1 0 1 0
green open .xerj_passkeys bc7b8a13-4b5d-4fd3-84ec-def70c401849 1 0 0 0
green open .xerj_idp_config 603ec211-fe9f-42a3-b771-992d2b9ecd79 1 0 0 0
green open .xerj_audit f6c3518c-439d-4610-8416-6dd2aee667b8 1 0 0 0
green open .xerj_alert_rules f8aaa9ca-797b-4a78-829a-87065e4ffa8e 1 0 0 0
green open .xerj_alert_fires 6fba0fa8-1690-4ce3-9a67-45a144807275 1 0 0 0
green open ssh-auth 2c8cd8a4-3f50-40a8-9dfa-5a835d97dd5d 1 0 655147 0
green open .xerj_users a2903ed4-ff14-499d-8c25-9ebe72f63056 1 0 0 0
```

## 4. Event distribution — what's actually in the file

```
$ for ev in other auth_failure_known_user conn_closed_preauth \
            auth_failure_invalid_user possible_break_in invalid_user auth_success; do
    curl -s localhost:9200/ssh-auth/_count -H "content-type: application/json" \
      -d "{\"query\":{\"term\":{\"event\":\"$ev\"}}}" | jq -c ".count as \$c | {\"$ev\": \$c}"
  done
{"other":354110}
{"auth_failure_known_user":177735}
{"conn_closed_preauth":68958}
{"auth_failure_invalid_user":19659}
{"possible_break_in":18909}
{"invalid_user":14392}
{"auth_success":182}
```

## 5. Top brute-force IPs

```
$ for ip in 119.7.221.129 103.99.0.122 5.188.10.182 5.188.10.156 42.159.145.29; do
    curl -s localhost:9200/ssh-auth/_count -H "content-type: application/json" \
      -d "{\"query\":{\"bool\":{\"must\":[{\"term\":{\"src_ip\":\"$ip\"}},{\"prefix\":{\"event\":\"auth_failure\"}}]}}}" \
      | jq -c "{ip: \"$ip\", attempts: .count}"
  done
{"ip":"119.7.221.129","attempts":1650}
{"ip":"103.99.0.122","attempts":930}
{"ip":"5.188.10.182","attempts":557}
{"ip":"5.188.10.156","attempts":464}
{"ip":"42.159.145.29","attempts":384}
```

## 6. Failed-logins-per-day timeline

```
$ for d in 2017-12-15 2017-12-22 2017-12-31 2018-01-04 2018-01-07; do
    next=$(date -d "$d +1 day" +%Y-%m-%d)
    curl -s localhost:9200/ssh-auth/_count -H "content-type: application/json" -d "{
      \"query\": { \"bool\": { \"must\": [
        { \"prefix\": { \"event\": \"auth_failure\" } },
        { \"range\":  { \"@timestamp\": { \"gte\": \"${d}T00:00:00Z\", \"lt\": \"${next}T00:00:00Z\" } } }
      ]}}}" | jq -c "{day: \"$d\", failures: .count}"
  done
{"day":"2017-12-15","failures":5131}
{"day":"2017-12-22","failures":1591}
{"day":"2017-12-31","failures":3356}
{"day":"2018-01-04","failures":0}
{"day":"2018-01-07","failures":0}
```

## 7. Actual time range of the loghub corpus

```
$ curl -s localhost:9200/ssh-auth/_search -H "content-type: application/json" -d "{
    \"size\":0, \"aggs\":{
      \"min_ts\":{\"min\":{\"field\":\"@timestamp\"}},
      \"max_ts\":{\"max\":{\"field\":\"@timestamp\"}}}}" | jq .aggregations
{
  "max_ts": {
    "value": 1483507344000.0,
    "value_as_string": "2017-01-04T05:22:24.000Z"
  },
  "min_ts": {
    "value": 1483507152000.0,
    "value_as_string": "2017-01-04T05:19:12.000Z"
  }
}
```

## 8. Full-text — find the spoofing attempts

```
$ curl -s localhost:9200/ssh-auth/_search -H 'content-type: application/json' -d '{
    "size": 3,
    "query": { "match_phrase": { "message": "POSSIBLE BREAK-IN ATTEMPT" } }
  }' | jq '.hits.total.value, .took, [.hits.hits[] | {ts:._source["@timestamp"], ip:._source.src_ip, msg:(._source.message[0:55]+"...")}]'
19406
2281
[
  {
    "ts": "2017-01-02T07:50:34Z",
    "ip": "218.65.30.30",
    "msg": "reverse mapping checking getaddrinfo for 30.30.65.218.b..."
  },
  {
    "ts": "2017-01-02T07:50:52Z",
    "ip": "218.65.30.30",
    "msg": "reverse mapping checking getaddrinfo for 30.30.65.218.b..."
  },
  {
    "ts": "2017-01-02T07:51:07Z",
    "ip": "218.65.30.30",
    "msg": "reverse mapping checking getaddrinfo for 30.30.65.218.b..."
  }
]
```

## 7b. Re-run failed-logins-per-day inside the actual loghub date range

```
Time range of the corpus:
{
  "max_ts": {
    "value": 1483507344000.0,
    "value_as_string": "2017-01-04T05:22:24.000Z"
  },
  "min_ts": {
    "value": 1483507152000.0,
    "value_as_string": "2017-01-04T05:19:12.000Z"
  }
}

$ for d in 2016-12-30 2017-01-02 2017-01-15 2017-02-15 2017-03-04; do
    next=$(date -d "$d +1 day" +%Y-%m-%d)
    curl -s localhost:9200/ssh-auth/_count ... 
    
{"day":"2016-12-30","failures":0}
{"day":"2017-01-02","failures":24724}
{"day":"2017-01-15","failures":0}
{"day":"2017-02-15","failures":0}
{"day":"2017-03-04","failures":0}
```

## 9. Footprint after the 655 K-doc ingest

```
$ ps -o pid,rss,vsz,etime,comm -p $(pgrep -f "target/release/xerj --insecure")
    PID   RSS    VSZ     ELAPSED COMMAND
  24532 519020 12874980    03:17 xerj

$ du -sh /path/to/data/ssh-auth
40M	/home/claude/ai/xerj/demo/.cli-validate/ssh-auth

$ ls -lh /home/claude/ai/xerj/engine/target/release/xerj
-rwxrwxr-x 2 claude claude 22M Apr 26 21:00 /home/claude/ai/xerj/engine/target/release/xerj
```

Notes captured live:
  - **RSS** ≈ 519 MB (`ps` reports kB → 519020 kB), CPU still 94 % from a background merge
  - **Disk for ssh-auth** = 40 MB segments (132 MB raw NDJSON in)
  - **Binary size** = 22 MB (single static-linked file)
  - **Compression ratio** = 132 MB → 40 MB = **3.3× smaller than raw**

## 10. Caveat: terms / date_histogram aggregations under-count

On this run, `_count` queries return correct totals (655147), but
`terms` aggregation on `event` returns only 256 docs (172 + 84) and
`date_histogram` returns the same. `_cat/segments` confirms one
merged segment with 655147 docs, so this looks like an aggregation-path
bug at v1.0.0-rc.1 that should be filed before GA. The §05/§06 demo
narrative should rely on `_count` for the "how many" numbers and only
use aggregations for shapes that demonstrably work.

## 11. Cold-start (5 runs, 5 ms poll resolution)

Times to first `/_cluster/health` 200, fresh data dir each run:

| run | ms |
|---|---|
| 1 | 47 |
| 2 | 63 |
| 3 | 47 |
| 4 | 57 |
| 5 | 45 |

**min 45 ms · median 47 ms**. The page's "4 ms" claim is the
engine's HTTP-listener bind time (sub-millisecond on Linux), not
the time until first request returns 200; we use the latter.

## Cleanup
- xerj was stopped cleanly (`kill -TERM`, exited within 100 ms)
- /tmp/xerj-cs and the .cli-validate data dir are left in place
  for re-inspection; remove with `rm -rf`
