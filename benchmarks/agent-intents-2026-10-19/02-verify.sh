jq -r 'to_entries[] | "\(.key)\t\(.value)"' claims.json | while IFS=$'\t' read -r field value; do
  q=$(jq -nc --arg v "$value" '{size:1, _source:["ax_path","ax_locator","body"], query:{match_phrase:{body:$v}}}')
  curl -s 'localhost:9200/web-*/_search' -H 'content-type: application/json' -d "$q" |
  jq -r --arg f "$field" --arg v "$value" '
    .hits.hits[0]._source as $s
    | if $s then
        ($s.body | ascii_downcase | index($v | ascii_downcase)) as $i
        | {field:$f, value:$v, found:true, source:"\($s.ax_path)#\($s.ax_locator)",
           evidence:($s.body[([$i-40,0]|max):($i+($v|length)+40)] | gsub("\\s+";" "))}
      else {field:$f, value:$v, found:false, source:null, evidence:null} end | tostring'
done
