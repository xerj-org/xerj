ids=$(jq -R . ids.txt | jq -sc .)
curl -s 'localhost:9200/xc-rust-vulns-*/_search' -H 'content-type: application/json' -d "{
  \"size\": 100,
  \"_source\": [\"id\", \"aliases\", \"packages\", \"fixed_versions\", \"origin\", \"source_url\"],
  \"query\": {\"bool\": {\"should\": [{\"terms\": {\"id\": $ids}}, {\"terms\": {\"aliases\": $ids}}]}}
}" | jq -r --argjson ids "$ids" '
  [.hits.hits[]._source] as $h
  | $ids[] as $q
  | ($h | map(select(.id == $q or ((.aliases // []) | index($q)))) | first) as $r
  | if $r then "\($q)  \($r.id)  \(($r.packages // []) | join(","))  fixed \(($r.fixed_versions // []) | join(","))  src \($r.source_url) :: \($r.origin)"
    else "\($q)  NOT FOUND in this pack: abstain" end'
