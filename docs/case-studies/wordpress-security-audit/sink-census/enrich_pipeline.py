#!/usr/bin/env python3
"""AI-enrichment pipeline for the sink census — the embedding-pipeline analogue,
but an AI agent (not a model.encode) writes SECURITY verdicts back into XERJ.

Flow per site:  pull candidate -> agent reads arg + enclosing code -> verdict
                (reachable / guarded / severity / note) -> update the wpsinks doc.

This file carries the agent's verdicts for the RARE high-risk classes (the ones
an auditor must review 100%). For the long tail (XSS/LFI) the same loop runs with
the taint graph deciding reachability; here we demonstrate + persist the
must-review verdicts so `wpsinks` becomes a queryable, enriched audit ledger.
"""
import json, urllib.request
X = "http://127.0.0.1:9200"

# ── Agent verdicts, keyed by file:line (reachable / guarded / severity / note) ──
V = {
 # RCE-command
 "wp-includes/class-snoopy.php:1018": ("server-http-lib-if-URI-attacker","escapeshellarg-NOT-option-safe","medium","OPTION INJECTION: $URI placed after curl flags with NO `--` separator; escapeshellarg stops command breakout but not curl option parsing -> $URI=\"-o/path\" writes a file (RCE) / -K reads. Snoopy legacy. Fix: add `--` before $URI."),
 "wp-includes/block-template-utils.php:1494": ("theme-export","ziparchive-CREATE-not-extract","low","ZipArchive::open(CREATE|OVERWRITE) builds an export zip; addFile/addFromString add content -> NOT zip-slip (that is on extractTo). WP extraction (unzip_file) hand-rolls path checks, avoiding extractTo."),
 "wp-includes/class-wp-image-editor-imagick.php:158": ("image-editor-uploaded-content","imagemagick-policy-dependent","medium","Imagick::readImage($this->file) parses attacker-UPLOADED image content -> ImageTragick/MVG/MSL/SVG delegate abuse (SSRF/RCE) if ImageMagick unpatched or policy.xml missing. WP checks file type first; content is still parsed."),
 "wp-includes/class-wp-image-editor-imagick.php:156": ("image-editor-uploaded-content","imagemagick-policy-dependent","medium","Imagick::readImageBlob(file_get_contents($this->file)) — same ImageTragick surface on uploaded content."),
 "wp-includes/ID3/getid3.lib.php:1806": ("media-analysis","fixed-binary+internal","medium","$commandline built from configured binary paths; confirm filenames are escaped"),
 "wp-includes/ID3/getid3.php:484": ("media-analysis","fixed-binary+internal","medium","same getID3 external-tool path"),
 "wp-includes/ID3/getid3.php:1824": ("media-analysis","fixed-binary+internal","medium","same"),
 "wp-includes/ID3/getid3.php:1835": ("media-analysis","fixed-binary+internal","medium","same"),
 "wp-includes/Text/Diff/Engine/shell.php:50": ("rare-nondefault-engine","filenames-concat","medium","from/to temp filenames concatenated into shell cmd; verify escaping; non-default diff engine"),
 "wp-includes/PHPMailer/PHPMailer.php:1937": ("mail-config","escapeshellcmd(sendmail)","low","$sendmail is the configured sendmail path"),
 "wp-includes/PHPMailer/PHPMailer.php:1965": ("mail-config","escapeshellcmd(sendmail)","low","same"),
 "wp-admin/includes/class-wp-debug-data.php:823": ("admin-site-health","literal-command","none","exec('gs --version') — no input"),
 "wp-admin/includes/class-ftp.php:912": ("ftp-lib-init","fixed-extension-name","low","dl() loads the fixed 'sockets' extension; name is a constant, not input"),
 "wp-admin/includes/class-wp-filesystem-ssh2.php:221": ("admin-filesystem","internal-fs-abstraction","medium","ssh2_exec($command) — command built by WP filesystem layer for admin file ops; not direct request input"),
 # SQLi (all wpdb driver-level)
 "wp-includes/class-wpdb.php:935": ("all-db","upstream-prepare","low","driver executor; injection safety is upstream in $wpdb->prepare"),
 "wp-includes/class-wpdb.php:951": ("all-db","literal","none","literal SELECT @@SESSION.sql_mode"),
 "wp-includes/class-wpdb.php:985": ("all-db","allow-list","none","$modes_str from a fixed WP sql_mode list, not user input"),
 "wp-includes/class-wpdb.php:2124": ("all-db","literal","none","literal 'DO 1' keep-alive"),
 "wp-includes/class-wpdb.php:2351": ("all-db","upstream-prepare","low","driver executor; safety upstream"),
 # deserialization
 "wp-includes/functions.php:655": ("meta/option-trusted","is_serialized+maybe_serialize-symmetry","low","maybe_unserialize; WP convention feeds only self-serialized trusted data"),
 "wp-includes/class-wp-customize-widgets.php:1498": ("customizer-auth","HMAC(wp_hash)+hash_equals","low","gate VERIFIED this session: unserialize only on valid secret-key hash"),
 "wp-includes/rss.php:809": ("legacy-magpie-cache","none","medium","deprecated Magpie RSS cache; POI if cache file poisoned"),
 "wp-includes/SimplePie/src/Cache/Memcache.php:96": ("feed-cache-backend","simplepie-filterediterator","medium","feed cache POI if attacker controls cache backend/content"),
 "wp-includes/SimplePie/src/Cache/Redis.php:119": ("feed-cache-backend","simplepie-filterediterator","medium","same"),
 "wp-includes/SimplePie/src/Cache/Memcached.php:92": ("feed-cache-backend","simplepie-filterediterator","medium","same"),
 "wp-includes/SimplePie/src/Cache/File.php:88": ("feed-cache-file","simplepie-filterediterator","medium","reads cache file then unserialize; POI if cache dir writable"),
 "wp-includes/SimplePie/src/Cache/MySQL.php:243": ("feed-cache-db","simplepie-filterediterator","medium","same"),
 "wp-includes/SimplePie/src/Cache/MySQL.php:274": ("feed-cache-db","simplepie-filterediterator","medium","same"),
 "wp-includes/blocks/legacy-widget.php:44": ("REST-auth-optin","show_instance_in_rest-gate","medium","widget instance deser; verify opt-in/hash gate like customizer"),
 "wp-includes/rest-api/endpoints/class-wp-rest-widgets-controller.php:589": ("REST-auth-optin","show_instance_in_rest-gate","medium","same widget-instance path"),
 "wp-includes/rest-api/endpoints/class-wp-rest-widget-types-controller.php:498": ("REST-auth-optin","show_instance_in_rest-gate","medium","same"),
 "wp-admin/includes/upgrade.php:2547": ("admin-upgrade","trusted-migration","low","DB upgrade routine; option value trusted"),
}

def post(path, body, ct="application/json"):
    r = urllib.request.Request(f"{X}/{path}", data=body.encode(),
                               headers={"Content-Type": ct}, method="POST")
    return json.loads(urllib.request.urlopen(r).read())

# pull every must-review site (the high-risk classes) and enrich — size covers all
q = {"query": {"terms": {"class": ["RCE-command","RCE-code","deserialization","SSRF","SQLi",
                                    "file-write","file-read","XXE","variable-injection",
                                    "dynamic-invoke","LDAP-injection","zip-slip-path-traversal","image-rce-ssrf"]}},
     "size": 5000, "_source": ["file","line","fn","class"]}
hits = post("wpsinks/_search", json.dumps(q))["hits"]["hits"]
reviewed = updates = 0
bulk = []
for h in hits:
    s = h["_source"]; key = f"{s['file']}:{s['line']}"
    if key in V:
        reach, guard, sev, note = V[key]; reviewed += 1
    else:
        reach, guard, sev, note = "unreviewed", "unknown", "review", "queued for agent read"
    bulk.append(json.dumps({"update": {"_id": h["_id"]}}))
    bulk.append(json.dumps({"doc": {"reachable": reach, "guarded": guard, "severity": sev,
                                    "ai_verdict": note, "enriched": True}}))
    updates += 1
resp = post("wpsinks/_bulk", "\n".join(bulk) + "\n", "application/x-ndjson")
print(f"enriched {updates} high-risk sink sites (errors={resp.get('errors')})")
print(f"  agent-reviewed with a full verdict: {reviewed}")
print(f"    (100% of the RARE classes: RCE-command 11/11, deserialization 13/13,")
print(f"     + the core wpdb-driver SQL sites; larger classes carry class+severity")
print(f"     and are queued for per-site agent read)")
print(f"  remaining high-risk queued: {updates - reviewed}")
