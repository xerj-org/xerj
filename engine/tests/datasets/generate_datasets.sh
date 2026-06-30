#!/usr/bin/env bash
# =============================================================================
# generate_datasets.sh — Generate realistic test datasets for xerj battle testing
#
# Produces four NDJSON files:
#   access_logs.ndjson  — 10 000 web server access log events
#   products.ndjson     — 5 000 e-commerce product documents
#   error_logs.ndjson   — 20 000 application log events (mix of levels)
#   articles.ndjson     — 1 000 Wikipedia-style articles
#
# Usage:
#   bash generate_datasets.sh          # generates all four files in PWD
#   bash generate_datasets.sh --quick  # tiny set (100 / 50 / 200 / 10) for CI
#
# Dependencies: bash 4+, awk, date (GNU coreutils or compatible macOS variant)
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${SCRIPT_DIR}"

QUICK=0
for arg in "$@"; do
    [[ "$arg" == "--quick" ]] && QUICK=1
done

if [[ $QUICK -eq 1 ]]; then
    ACCESS_N=100; PRODUCT_N=50; ERROR_N=200; ARTICLE_N=10
    echo "[quick mode] generating small datasets for CI"
else
    ACCESS_N=10000; PRODUCT_N=5000; ERROR_N=20000; ARTICLE_N=1000
fi

# ---------------------------------------------------------------------------
# Utility: ISO-8601 timestamp offset by $2 seconds from a base epoch
# ---------------------------------------------------------------------------
# We generate timestamps spread across the last 72 hours.
BASE_EPOCH=$(date +%s 2>/dev/null || echo 1704067200)

ts_offset() {
    local off=$1
    local epoch=$(( BASE_EPOCH - off ))
    # Portable ISO-8601 — try GNU date, fall back to python, fall back to awk
    if date --version >/dev/null 2>&1; then
        date -u -d "@${epoch}" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || \
        python3 -c "import datetime; print(datetime.datetime.utcfromtimestamp(${epoch}).strftime('%Y-%m-%dT%H:%M:%SZ'))"
    else
        # macOS BSD date
        date -u -r "${epoch}" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || \
        python3 -c "import datetime; print(datetime.datetime.utcfromtimestamp(${epoch}).strftime('%Y-%m-%dT%H:%M:%SZ'))"
    fi
}

# ---------------------------------------------------------------------------
# Pure-awk NDJSON generator — no external JSON lib needed
# ---------------------------------------------------------------------------
# All four generators are written as awk programs for portability and speed.

# ===========================================================================
# A) Web server access logs — 10 000 lines
# ===========================================================================
echo "Generating access_logs.ndjson (${ACCESS_N} lines)..."

awk -v n="${ACCESS_N}" -v base_epoch="${BASE_EPOCH}" '
BEGIN {
    srand(42)

    split("GET POST PUT DELETE PATCH", methods, " ")
    split("200 200 200 200 200 200 301 302 400 401 403 404 404 404 500 500 502 503", statuses, " ")

    split("/api/users /api/products /api/orders /api/cart /api/search /login /logout /register /dashboard /profile /settings /admin /api/auth/token /api/recommendations /api/inventory /static/js/app.js /static/css/main.css /static/img/logo.png /health /metrics", paths, " ")

    split("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36 Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/115.0 curl/7.88.1 python-requests/2.31.0 Go-http-client/1.1 Googlebot/2.1 (+http://www.google.com/bot.html)", agents, " ")

    n_methods = 5; n_statuses = 18; n_paths = 20

    for (i = 1; i <= n; i++) {
        # Timestamp: spread over last 72h, recent events more likely
        off = int(rand() * rand() * 259200)  # long-tail toward recent
        epoch = base_epoch - off
        # Format timestamp manually (portable)
        secs = epoch % 86400
        days = int(epoch / 86400)
        # Approximate date from epoch (good enough for test data)
        # Use a fixed base: 2024-01-15 = epoch day 19737
        y = 2024; m = 1; d = 15
        extra_days = days - 19737
        d = d + extra_days
        # Simple overflow handling
        while (d > 28) { d -= 28; m++ }
        while (m > 12) { m -= 12; y++ }
        hh = int(secs / 3600); mm = int((secs % 3600) / 60); ss = secs % 60

        ts = sprintf("%04d-%02d-%02dT%02d:%02d:%02dZ", y, m, d, hh, mm, ss)

        method = methods[int(rand() * n_methods) + 1]
        path   = paths[int(rand() * n_paths) + 1]
        status = statuses[int(rand() * n_statuses) + 1]

        # Response time: long-tail (most fast, some slow)
        rt = int(exp(rand() * 8.5))  # 1..~5000ms
        if (rt > 5000) rt = 5000
        if (rt < 1)    rt = 1

        # Bytes: correlated with status
        if (status == 200) {
            bytes = 500 + int(rand() * 15000)
        } else if (status ~ /^3/) {
            bytes = 0
        } else {
            bytes = 100 + int(rand() * 500)
        }

        # Client IP — private ranges
        ip = sprintf("10.%d.%d.%d", int(rand()*3), int(rand()*256), int(rand()*256))

        # User-agent index
        ua_i = (int(rand() * 6) + 1)
        if      (ua_i == 1) ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36"
        else if (ua_i == 2) ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/119.0.0.0 Safari/537.36"
        else if (ua_i == 3) ua = "Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/115.0"
        else if (ua_i == 4) ua = "curl/7.88.1"
        else if (ua_i == 5) ua = "python-requests/2.31.0"
        else                 ua = "Go-http-client/1.1"

        # Request ID
        rid = sprintf("%08x-%04x", int(rand()*4294967295), int(rand()*65535))

        printf "{\"@timestamp\":\"%s\",\"method\":\"%s\",\"path\":\"%s\",\"status\":%s,\"response_time_ms\":%d,\"client_ip\":\"%s\",\"user_agent\":\"%s\",\"bytes\":%d,\"request_id\":\"%s\"}\n",
               ts, method, path, status, rt, ip, ua, bytes, rid
    }
}
' /dev/null > "${OUT_DIR}/access_logs.ndjson"

echo "  -> $(wc -l < "${OUT_DIR}/access_logs.ndjson") lines written"

# ===========================================================================
# B) E-commerce product catalog — 5 000 products
# ===========================================================================
echo "Generating products.ndjson (${PRODUCT_N} products)..."

awk -v n="${PRODUCT_N}" '
BEGIN {
    srand(1337)

    split("electronics clothing footwear home-garden sports-outdoors toys-games books health-beauty automotive food-beverages office-supplies pet-supplies musical-instruments art-crafts jewelry watches baby luggage tools industrial", categories, " ")
    n_cat = 20

    split("Acme TechPro UltraGear ProMax NexGen OmniCore PureTech SkyLine VertexX BlueStar RedWave GoldPeak IronForge SilverShield DiamondEdge CrystalPure VortexX HorizonX ZenithX ApexPro", brands, " ")
    n_brands = 20

    split("fast delivery premium quality eco-friendly sale new bestseller limited refurbished bundle warranty-included handmade exclusive certified organic vegan cruelty-free professional grade budget-friendly compact lightweight", tags_pool, " ")
    n_tags = 20

    split("high quality durable excellent craftsmanship lightweight easy to use versatile reliable long-lasting innovative design premium materials efficient performance outstanding results superior build trusted brand", adj_pool, " ")
    n_adj = 16

    # 50 cities for geo distribution
    split("40.7128,-74.0060 34.0522,-118.2437 41.8781,-87.6298 29.7604,-95.3698 33.4484,-112.0740 39.9526,-75.1652 29.4241,-98.4936 32.7157,-117.1611 32.7767,-96.7970 37.3382,-121.8863 47.6062,-122.3321 25.7617,-80.1918 39.7392,-104.9903 30.3322,-81.6557 35.4676,-97.5164 36.1627,-86.7816 45.5231,-122.6765 38.8951,-77.0364 35.2271,-80.8431 44.9778,-93.2650", cities, " ")
    n_cities = 20

    split("This product offers remarkable value with its premium construction and thoughtful design. Perfect for everyday use and special occasions alike. Built to last with quality materials. A customer favorite with thousands of five-star reviews. Industry-leading performance at an unbeatable price point. Engineered for professionals who demand the best. Compact and portable without sacrificing functionality. The perfect gift for friends and family. Trusted by experts worldwide for reliability and innovation. Elevate your daily routine with this must-have item.", sentences, ". ")
    n_sentences = 10

    for (i = 1; i <= n; i++) {
        cat = categories[int(rand() * n_cat) + 1]
        brand = brands[int(rand() * n_brands) + 1]

        # Product name
        adj = adj_pool[int(rand() * n_adj) + 1]
        name = brand " " adj " " cat " Model-" sprintf("%04d", i)

        # Price — log-normal-ish distribution $1-$5000
        raw = exp(rand() * 8.5)
        if (raw < 1)    raw = 1
        if (raw > 5000) raw = 5000
        price = int(raw * 100) / 100.0

        # Rating
        rating_raw = 1.0 + rand() * 4.0
        rating = int(rating_raw * 10) / 10.0

        in_stock = (rand() > 0.15) ? "true" : "false"

        # Tags (2-4 random)
        n_t = int(rand() * 3) + 2
        tags = ""
        for (t = 0; t < n_t; t++) {
            tag = tags_pool[int(rand() * n_tags) + 1]
            if (t == 0) tags = "\"" tag "\""
            else        tags = tags ",\"" tag "\""
        }

        # Description: 3-6 sentences
        n_s = int(rand() * 4) + 3
        desc = ""
        for (s = 0; s < n_s; s++) {
            si = int(rand() * n_sentences) + 1
            desc = desc sentences[si] ". "
        }
        # Escape quotes in description
        gsub(/"/, "\\\"", desc)
        gsub(/\n/, " ", desc)

        # Geo: pick a city, add small jitter
        city_pair = cities[int(rand() * n_cities) + 1]
        split(city_pair, ll, ",")
        lat = ll[1] + (rand() - 0.5) * 0.5
        lon = ll[2] + (rand() - 0.5) * 0.5

        # SKU and review count
        sku = sprintf("SKU-%06d", i)
        reviews = int(rand() * 5000)

        printf "{\"id\":%d,\"sku\":\"%s\",\"name\":\"%s\",\"description\":\"%s\",\"price\":%.2f,\"category\":\"%s\",\"brand\":\"%s\",\"rating\":%.1f,\"review_count\":%d,\"in_stock\":%s,\"tags\":[%s],\"location\":{\"lat\":%.4f,\"lon\":%.4f}}\n",
               i, sku, name, desc, price, cat, brand, rating, reviews, in_stock, tags, lat, lon
    }
}
' /dev/null > "${OUT_DIR}/products.ndjson"

echo "  -> $(wc -l < "${OUT_DIR}/products.ndjson") lines written"

# ===========================================================================
# C) Application error logs — 20 000 lines
# ===========================================================================
echo "Generating error_logs.ndjson (${ERROR_N} lines)..."

awk -v n="${ERROR_N}" -v base_epoch="${BASE_EPOCH}" '
BEGIN {
    srand(9001)

    split("auth-svc api-gateway payment-svc inventory-svc notification-svc", services, " ")
    n_services = 5

    split("prod-web-01 prod-web-02 prod-web-03 prod-api-01 prod-api-02 prod-db-01 prod-db-02 prod-cache-01 prod-worker-01 prod-worker-02", hosts, " ")
    n_hosts = 10

    # Level distribution: 60% INFO, 20% WARN, 15% ERROR, 5% FATAL
    split("INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO INFO WARN WARN WARN WARN WARN WARN WARN WARN ERROR ERROR ERROR ERROR ERROR ERROR FATAL FATAL", levels, " ")
    n_levels = 36

    # INFO messages (indexed 1..10)
    info_msgs[1]  = "Request processed successfully"
    info_msgs[2]  = "User authenticated successfully"
    info_msgs[3]  = "Database connection pool initialized"
    info_msgs[4]  = "Cache hit ratio: 98.5%"
    info_msgs[5]  = "Background job completed"
    info_msgs[6]  = "Sending notification to user"
    info_msgs[7]  = "Health check passed"
    info_msgs[8]  = "Configuration reloaded successfully"
    info_msgs[9]  = "Session created for user"
    info_msgs[10] = "Index refresh completed"

    # WARN messages (indexed 1..8)
    warn_msgs[1] = "High memory usage detected: 85%"
    warn_msgs[2] = "Slow query detected: 2300ms"
    warn_msgs[3] = "Response time degraded for endpoint"
    warn_msgs[4] = "Retry attempt 2 of 3"
    warn_msgs[5] = "Connection pool near capacity"
    warn_msgs[6] = "Circuit breaker in half-open state"
    warn_msgs[7] = "Cache eviction rate elevated"
    warn_msgs[8] = "Disk usage at 78%"

    # ERROR messages (indexed 1..10)
    error_msgs[1]  = "Connection refused to database at 10.0.0.5:5432"
    error_msgs[2]  = "Authentication token expired or invalid"
    error_msgs[3]  = "Failed to process payment: gateway timeout"
    error_msgs[4]  = "NullPointerException in OrderService.process"
    error_msgs[5]  = "Queue consumer fell behind by 5000 messages"
    error_msgs[6]  = "Redis connection timeout after 30s"
    error_msgs[7]  = "Failed to send email: SMTP error 550"
    error_msgs[8]  = "Unhandled exception in request pipeline"
    error_msgs[9]  = "Database deadlock detected and rolled back"
    error_msgs[10] = "S3 upload failed: access denied"

    # FATAL messages (indexed 1..4)
    fatal_msgs[1] = "Out of memory: kill process or sacrifice child"
    fatal_msgs[2] = "Panic: index out of bounds in segment merger"
    fatal_msgs[3] = "Segmentation fault in native library"
    fatal_msgs[4] = "Unrecoverable disk I/O error on /data"

    n_info = 10; n_warn = 8; n_error = 10; n_fatal = 4

    for (i = 1; i <= n; i++) {
        off = int(rand() * 259200)
        epoch = base_epoch - off
        # Simple date calc
        secs = epoch % 86400
        days = int(epoch / 86400)
        y = 2024; m = 1; d = 15
        extra_days = days - 19737
        d = d + extra_days
        while (d > 28) { d -= 28; m++ }
        while (m > 12) { m -= 12; y++ }
        hh = int(secs / 3600); mm2 = int((secs % 3600) / 60); ss = secs % 60
        ts = sprintf("%04d-%02d-%02dT%02d:%02d:%02dZ", y, m, d, hh, mm2, ss)

        level   = levels[int(rand() * n_levels) + 1]
        service = services[int(rand() * n_services) + 1]
        host    = hosts[int(rand() * n_hosts) + 1]

        # Pick message based on level
        if (level == "INFO") {
            msg = info_msgs[int(rand() * n_info) + 1]
            stack = ""
        } else if (level == "WARN") {
            msg = warn_msgs[int(rand() * n_warn) + 1]
            stack = ""
        } else if (level == "ERROR") {
            msg = error_msgs[int(rand() * n_error) + 1]
            stack = sprintf("at %s.handle(Request.java:142)\\n  at com.xerj.%s.Server.run(Server.java:88)", service, service)
        } else {
            msg = fatal_msgs[int(rand() * n_fatal) + 1]
            stack = sprintf("FATAL in %s\\n  core dumped to /var/crash/%s.core", service, host)
        }

        if (msg == "") msg = "Unknown event in " service

        # Trace ID
        trace = sprintf("%08x%08x", int(rand()*4294967295), int(rand()*4294967295))
        span  = sprintf("%08x", int(rand()*4294967295))

        # Duration ms
        dur = int(rand() * 2000)

        # Escape message
        gsub(/"/, "\\\"", msg)
        gsub(/"/, "\\\"", stack)

        if (stack != "") {
            printf "{\"@timestamp\":\"%s\",\"level\":\"%s\",\"service\":\"%s\",\"message\":\"%s\",\"trace_id\":\"%s\",\"span_id\":\"%s\",\"hostname\":\"%s\",\"duration_ms\":%d,\"stack_trace\":\"%s\"}\n",
                   ts, level, service, msg, trace, span, host, dur, stack
        } else {
            printf "{\"@timestamp\":\"%s\",\"level\":\"%s\",\"service\":\"%s\",\"message\":\"%s\",\"trace_id\":\"%s\",\"span_id\":\"%s\",\"hostname\":\"%s\",\"duration_ms\":%d}\n",
                   ts, level, service, msg, trace, span, host, dur
        }
    }
}
' /dev/null > "${OUT_DIR}/error_logs.ndjson"

echo "  -> $(wc -l < "${OUT_DIR}/error_logs.ndjson") lines written"

# ===========================================================================
# D) Wikipedia-like articles — 1 000 docs
# ===========================================================================
echo "Generating articles.ndjson (${ARTICLE_N} articles)..."

awk -v n="${ARTICLE_N}" -v base_epoch="${BASE_EPOCH}" '
BEGIN {
    srand(2718)

    split("Programming Language Database System Operating System Algorithm Data Structure Networking Protocol Compiler Design Machine Learning Neural Network Computer Architecture Cloud Computing Distributed Systems Cryptography Software Engineering Web Framework Version Control System Container Technology", topic_suffixes, " ")
    n_topics = 20

    split("Rust Python Go TypeScript Kotlin Swift C++ Java Scala Haskell Erlang Elixir Clojure OCaml F# Lua Julia R MATLAB Zig", lang_prefixes, " ")
    n_langs = 20

    split("Dr. Alice Johnson Prof. Bob Martinez Dr. Carol Chen Prof. David Kim Dr. Emma Wilson Prof. Frank Lee Dr. Grace Park Prof. Henry Brown Dr. Isabel Martinez Prof. James Anderson", authors, " ")
    n_authors = 10

    split("systems-programming web-development databases distributed-systems security compilers algorithms artificial-intelligence networking cloud", tag_pool, " ")
    n_tag_pool = 10

    # Paragraph templates
    para1 = "is a widely adopted technology first introduced in the early days of modern computing. Its design philosophy emphasizes correctness, performance, and developer productivity. The community around it has grown substantially over the past decade."

    para2 = "The core architecture consists of several key components: a runtime environment that manages resources, a standard library providing common abstractions, and a toolchain that compiles source code into optimized executables. Performance benchmarks consistently place it among the top-tier solutions."

    para3 = "One of the distinguishing features is its memory safety guarantees. Unlike older systems that rely on manual memory management, this approach prevents entire classes of bugs including buffer overflows, use-after-free errors, and data races. These properties make it particularly attractive for security-critical applications."

    para4 = "The ecosystem includes a rich collection of third-party libraries and frameworks. Package management is handled through a centralized registry that hosts thousands of open-source contributions. The build system integrates seamlessly with continuous integration pipelines and deployment workflows."

    para5 = "Adoption has been rapid across industry verticals including finance, healthcare, and infrastructure. Major technology companies have contributed significant resources to its development and maintenance. The governance model ensures long-term stability while allowing the community to drive innovation."

    para6 = "Performance characteristics vary by workload. Computational tasks that benefit from parallelism see near-linear scaling across multiple cores. I/O-bound workloads achieve throughput comparable to hand-optimized C code in production environments. Latency percentiles (p99) remain consistently low under high concurrency."

    para7 = "Security considerations are a first-class concern. The type system prevents entire categories of vulnerabilities at compile time. Runtime checks catch the remainder. Formal verification tools can be applied to critical code paths for high-assurance environments such as avionics and medical devices."

    para8 = "The learning curve is acknowledged to be steeper than scripting languages but comparable to other systems-level technologies. Extensive documentation, interactive tutorials, and a welcoming community help newcomers become productive within weeks. University curricula increasingly include it as a foundational subject."

    for (i = 1; i <= n; i++) {
        lang   = lang_prefixes[int(rand() * n_langs) + 1]
        topic  = topic_suffixes[int(rand() * n_topics) + 1]
        title  = lang " " topic

        author = authors[int(rand() * n_authors) + 1]

        # Tags: 2-4
        n_t = int(rand() * 3) + 2
        tags = ""
        used[0] = 0  # reset
        for (t = 0; t < n_t; t++) {
            tag = tag_pool[int(rand() * n_tag_pool) + 1]
            if (t == 0) tags = "\"" tag "\""
            else        tags = tags ",\"" tag "\""
        }

        # Body: 4-8 paragraphs
        n_p = int(rand() * 5) + 4
        body = title " " para1 " " para2 " " para3
        if (n_p >= 4) body = body " " para4
        if (n_p >= 5) body = body " " para5
        if (n_p >= 6) body = body " " para6
        if (n_p >= 7) body = body " " para7
        if (n_p >= 8) body = body " " para8
        gsub(/"/, "\\\"", body)
        gsub(/\n/, " ", body)

        # Word count (approximate)
        wc = split(body, tmp, " ")

        # Published date
        off = int(rand() * 31536000)  # within last year
        epoch = base_epoch - off
        secs = epoch % 86400
        days = int(epoch / 86400)
        y = 2024; m = 1; d = 15
        extra_days = days - 19737
        d = d + extra_days
        while (d > 28) { d -= 28; m++ }
        while (m > 12) { m -= 12; y++ }
        hh = int(secs / 3600); mm2 = int((secs % 3600) / 60)
        pub = sprintf("%04d-%02d-%02dT%02d:%02d:%02dZ", y, m, d, hh, mm2, 0)

        # Views
        views = int(rand() * 1000000)

        gsub(/"/, "\\\"", title)
        gsub(/"/, "\\\"", author)

        printf "{\"id\":%d,\"title\":\"%s\",\"body\":\"%s\",\"author\":\"%s\",\"published_at\":\"%s\",\"tags\":[%s],\"word_count\":%d,\"views\":%d}\n",
               i, title, body, author, pub, tags, wc, views
    }
}
' /dev/null > "${OUT_DIR}/articles.ndjson"

echo "  -> $(wc -l < "${OUT_DIR}/articles.ndjson") lines written"

# ===========================================================================
# Summary
# ===========================================================================
echo ""
echo "Dataset generation complete:"
echo "  access_logs.ndjson  : $(wc -l < "${OUT_DIR}/access_logs.ndjson") lines  ($(du -sh "${OUT_DIR}/access_logs.ndjson" | cut -f1))"
echo "  products.ndjson     : $(wc -l < "${OUT_DIR}/products.ndjson") lines  ($(du -sh "${OUT_DIR}/products.ndjson" | cut -f1))"
echo "  error_logs.ndjson   : $(wc -l < "${OUT_DIR}/error_logs.ndjson") lines  ($(du -sh "${OUT_DIR}/error_logs.ndjson" | cut -f1))"
echo "  articles.ndjson     : $(wc -l < "${OUT_DIR}/articles.ndjson") lines  ($(du -sh "${OUT_DIR}/articles.ndjson" | cut -f1))"
echo ""
echo "Total: $(( $(wc -l < "${OUT_DIR}/access_logs.ndjson") + $(wc -l < "${OUT_DIR}/products.ndjson") + $(wc -l < "${OUT_DIR}/error_logs.ndjson") + $(wc -l < "${OUT_DIR}/articles.ndjson") )) documents"
