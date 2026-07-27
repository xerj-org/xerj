#!/usr/bin/env python3
"""Verify Finding 1 by EXECUTING the flow (no PHP runtime needed).

Line-for-line port of WordPress `wp_http_validate_url` (wp-includes/http.php):
same scheme filter, same strpbrk host check, same IPv4 octet-range test, using
real DNS via socket.gethostbyname (== PHP gethostbyname). Run it and see the
169.254.0.0/16 cloud-metadata range slip through the deny-list."""
import re, socket
from urllib.parse import urlparse

def strpbrk(s, chars): return any(c in s for c in chars)

def wp_http_validate_url(url, home="http://myblog.example"):
    if not isinstance(url, str) or url == "":
        return False
    p = urlparse(url)
    if p.scheme not in ("http", "https"):
        return False
    if not p.hostname:
        return False
    if p.username or p.password:
        return False
    host = p.hostname
    if strpbrk(host, ":#?[]"):
        return False
    parsed_home = urlparse(home)
    same_host = parsed_home.hostname and parsed_home.hostname.lower() == host.lower()
    host = host.strip(".")
    if not same_host:
        if re.match(r"^(([1-9]?\d|1\d\d|25[0-5]|2[0-4]\d)\.){3}"
                    r"([1-9]?\d|1\d\d|25[0-5]|2[0-4]\d)$", host):
            ip = host
        else:
            try:
                ip = socket.gethostbyname(host)
            except Exception:
                return False
            if ip == host:
                return False
        if ip:
            parts = [int(x) for x in ip.split(".")]
            if (127 == parts[0] or 10 == parts[0] or 0 == parts[0]
                    or (172 == parts[0] and 16 <= parts[1] <= 31)
                    or (192 == parts[0] and 168 == parts[1])):
                # http_request_host_is_external default false -> reject
                return False
    return url  # (port check omitted; default ports pass)

if __name__ == "__main__":
    tests = [
        "http://127.0.0.1/",
        "http://10.1.2.3/",
        "http://192.168.0.1/",
        "http://169.254.169.254/latest/meta-data/iam/security-credentials/",  # AWS
        "http://[::1]/",
        "http://example.com/",
    ]
    print("EXECUTING wp_http_validate_url on live payloads:\n")
    for u in tests:
        r = wp_http_validate_url(u)
        verdict = "REJECTED (SSRF blocked)" if r is False else "ALLOWED  --> request proceeds"
        print(f"  {verdict:32}  {u}")
    print("\nFinding: 169.254.0.0/16 (link-local, incl. 169.254.169.254 cloud "
          "metadata) is NOT in the deny-list -> ALLOWED.")
