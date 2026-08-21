---
title: "Search BVH motion-capture files"
h1: "How do I search BVH motion-capture files?"
description: "xerj autoindex detects the bvh family and extracts frames, frame_time_s, duration_s, joint_count and a joints keyword array. The family produces no body field."
slug: "search-bvh-motion-capture-files"
cluster: "Files and formats"
question: "How do I search motion-capture data?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install the latest XERJ and start a node, run xerj autoindex on a folder of .bvh files with --prefix bvh, read GET /bvh-*/_mapping, then find a clip by joint name with a terms query on joints and report frames, frame_time_s and duration_s for every hit."
commands:
  - cmd: "xerj autoindex ./bvh --url http://127.0.0.1:9480 --prefix bvh --state-dir ./state-bvh --progress plain --disable-feedback"
    note: "Index the folder of .bvh clips into bvh-* indices."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9480/bvh-*/_mapping'"
    note: "Read the 6 extracted fields before you write a query, because the family has no body field."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9480/bvh-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"terms\":{\"joints\":[\"LeftUpLeg\"]}},\"size\":10,\"_source\":[\"ax_path\",\"joints\",\"joint_count\",\"frames\",\"frame_time_s\",\"duration_s\"],\"track_total_hits\":true}'"
    note: "Find every clip that contains a named joint, and read its motion fields back."
links_out:
  - "search-unity-project-assets"
  - "search-file-contents-in-a-folder"
  - "catalog-files-with-autoindex-map"
faq:
  - q: "How do I search BVH motion-capture files?"
    a: "Run `xerj autoindex` on the folder. XERJ detects the `bvh` family and extracts `frames`, `frame_time_s`, `duration_s`, `joint_count` and a `joints` array."
  - q: "Which fields does the bvh family produce?"
    a: "Six beyond provenance: `title`, `frames`, `frame_time_s`, `duration_s`, `joint_count` and `joints`. Our clip reported 60 frames over 1.999998 seconds."
  - q: "How do I find a clip by joint name?"
    a: "Send a `terms` query with a 1-element list on `joints`. That returned 1 hit in our capture."
  - q: "Why does my match query on body return nothing?"
    a: "Because the `bvh` family produces no `body` field. Query the `joints` array instead, and read the mapping before you write anything else."
  - q: "Can I filter clips by length?"
    a: "Yes. `duration_s` and `frame_time_s` are `double` and `frames` is `long`, so a `range` query over any of them works directly."
  - q: "Does XERJ index the per-frame channel data?"
    a: "No. The extractor summarizes the clip. It writes the joint names and the frame counters, not the numeric motion rows underneath them."
  - q: "Is motion capture a priority format for XERJ?"
    a: "No. The coverage matrix rates BVH as curiosity value with negligible volume. The family works, and the demand behind it is small."
---

**TL;DR** — Run `xerj autoindex` on the folder. XERJ detects the `bvh` family and writes `frames`, `frame_time_s`, `duration_s`, `joint_count` and a `joints` array. Our clip reported 60 frames over 1.999998 seconds, and a `terms` query on `joints` returned it.

## Index the folder of clips

Install the latest XERJ first. Point `xerj autoindex` at the folder that holds the `.bvh` files. Detection is content-first, so the extension decides nothing.

```sh
xerj autoindex ./bvh --url http://127.0.0.1:9480 --prefix bvh --state-dir ./state-bvh --progress plain --disable-feedback
```

The capture indexed 1 clip and the catalog recorded it as `format=bvh status=indexed`. The `bvh-*` pattern then reaches every clip in one query.

## The fields a clip becomes

The `bvh` family writes 6 fields beyond the 7 provenance fields. The values below are the whole document for a 4-joint walk cycle.

```json
{"ax_path":"walk_cycle.bvh","ax_format":"bvh","ax_locator":"bvh",
 "title":"walk_cycle.bvh",
 "frames":60,
 "frame_time_s":0.0333333,
 "duration_s":1.999998,
 "joint_count":4,
 "joints":["Hips","LeftUpLeg","LeftLeg","Spine"]}
```

`frames` and `joint_count` are `long`. `frame_time_s` and `duration_s` are `double`, so a `range` query filters clips by length with no extra mapping work.

## Find a clip by joint name

We asked for the joint `LeftUpLeg` against the indexed clip. Both of the query shapes below returned it.

| query | hits |
| --- | --- |
| `terms` on `joints`, 1-element list | 1 |
| `wildcard` on `joints`, no wildcard character | 1 |

Use the `terms` form. It reads the `joints` array directly and needs no pattern syntax.

```sh
curl -s -XPOST 'http://127.0.0.1:9480/bvh-*/_search' -H 'content-type: application/json' -d '{"query":{"terms":{"joints":["LeftUpLeg"]}},"size":10,"_source":["ax_path","joints","joint_count","frames","frame_time_s","duration_s"],"track_total_hits":true}'
```

## There is no `body` field

The `bvh` family declares no `body` field. `{"match":{"body":"LeftUpLeg"}}` returned 0 hits against the clip, so read the mapping before you write a full-text query.

```sh
curl -s -XGET 'http://127.0.0.1:9480/bvh-*/_mapping'
```

The joint name is still in the document, in the `joints` array and in `_source`. Query `joints` for it.

## What the extractor does not do

The extractor summarizes a clip. XERJ writes the joint names and the frame counters, and indexes none of the numeric rows under the `MOTION` header.

There is no skeleton hierarchy field either. `joints` is a flat array, so a query cannot ask which joint is the parent of another.

## What the capture was

One single-node XERJ process and 1 generated fixture file of 8,085 bytes. Our fixture generator wrote that file to the BVH format, and no capture studio produced it.

In the coverage matrix, motion capture is a low priority. This page exists because the family is real, not because the demand is large.
