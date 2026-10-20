#!/usr/bin/env python3
"""S1 sample miner — validates the code-archaeology stratum of corpus-x100.

For each advisory seed (from the gen-1 rust-vulns pack) with a fix-commit
reference and a semver range:
  1. partial-clone the repo (blobs on demand),
  2. diff fix^..fix for changed *.rs files,
  3. extract the vulnerable file content at fix^ (always tree-aligned),
  4. split out the functions whose hunks touch them (regex-level split;
     the production harvester uses tree-sitter-rust),
  5. capture before/after regression tests (the trigger pair),
  6. count published crate versions in the affected range (version pinning).

Emits one JSON summary line per seed. This is the feasibility probe for
CORPUS-X100.md, not the production harvester.
"""
import json, re, subprocess, sys, tempfile, urllib.request, os, shutil
from collections import namedtuple

UA = {'User-Agent': 'xerj-corpus-x100-sample (contact: dev@xerj.org)'}

Seed = namedtuple('Seed', 'adv_id crate repo sha fixed')


def http_json(url):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())


def run(cmd, cwd):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd[:4])}… -> {p.returncode}: {p.stderr[:200]}")
    return p.stdout


def parse_url(u):
    # https://github.com/OWNER/REPO/commit/SHA -> (owner/repo, sha)
    m = re.match(r'https://github\.com/([^/]+)/([^/]+)/commit/([0-9a-f]{7,40})', u)
    return (f'{m.group(1)}/{m.group(2)}', m.group(3)) if m else None


FN_RE = re.compile(r'^(\s*)(pub(?:\([^)]*\))?\s+)?(async\s+)?fn\s+(\w+)', re.M)


def functions_touching(file_src, hunk_lines):
    """Functions whose declared line range contains any hunk line number."""
    out = []
    for m in FN_RE.finditer(file_src):
        start = file_src[:m.start()].count('\n') + 1
        body = file_src[m.start():]
        # brace-match from the fn's opening brace
        i, depth, opened = body.find('{'), 0, False
        end = start
        for j in range(i, len(body)):
            if body[j] == '{':
                depth, opened = depth + 1, True
            elif body[j] == '}':
                depth -= 1
                if opened and depth == 0:
                    end = start + body[:j].count('\n')
                    break
        if any(start <= ln <= end for ln in hunk_lines):
            out.append({'fn': m.group(4), 'line': start, 'source': body[:j + 1]})
    return out


def mine(seed, workdir):
    rec = {'advisory': seed.adv_id, 'crate': seed.crate, 'repo': seed.repo}
    d = os.path.join(workdir, re.sub(r'\W', '_', seed.repo))
    if not os.path.isdir(d):
        run(['git', 'clone', '--quiet', '--filter=blob:none', '--no-checkout',
             f'https://github.com/{seed.repo}', d], workdir)
    # resolve short shas
    sha = run(['git', 'rev-parse', seed.sha + '^{commit}'], d).strip()
    files = [f for f in run(['git', 'diff', '--name-only', sha + '^', sha, '--', '*.rs'],
                            d).splitlines() if f]
    rec['changed_rs'] = len(files)
    vuln_fns, tests = [], []
    for f in files:
        try:
            src = run(['git', 'show', f'{sha}^:{f}'], d)
        except RuntimeError:
            continue  # file added by the fix itself — no vulnerable parent copy
        hunks = run(['git', 'diff', '-U0', sha + '^', sha, '--', f], d)
        lines = []
        for h in re.finditer(r'^@@+\s+-(\d+)(?:,(\d+))?', hunks, re.M):
            a, n = int(h.group(1)), int(h.group(2) or 1)
            lines += list(range(a, a + n))
        for fn in functions_touching(src, lines):
            vuln_fns.append({'file': f, **{k: fn[k] for k in ('fn', 'line')},
                             'bytes': len(fn['source'])})
        if '/test' in f or f.startswith('tests/') or 'mod tests' in src:
            tests.append(f)
    # in-file test modules count as trigger sources too
    patch = run(['git', 'diff', sha + '^', sha], d)
    test_fns = sorted(set(re.findall(r'fn (test_\w+)', patch)))
    rec['vuln_functions'] = vuln_fns
    rec['trigger_tests_touched'] = len(test_fns)
    rec['trigger_tests'] = test_fns[:12]
    rec['patch_bytes'] = len(patch)
    # version pinning: published versions in range (approx: all <= fixed)
    try:
        vs = http_json(f'https://crates.io/api/v1/crates/{seed.crate}/versions')['versions']
        nums = [v['num'] for v in vs]
        in_range = [n for n in nums if n < (seed.fixed or '∞')] if seed.fixed else nums
        rec['published_versions'] = len(nums)
        rec['versions_le_fixed'] = len(in_range)
    except Exception as e:
        rec['version_error'] = str(e)[:80]
    return rec


def main():
    seeds = [Seed(*s) for s in json.load(open(sys.argv[1]))]
    total_fns = 0
    with tempfile.TemporaryDirectory(prefix='s1mine-') as wd:
        for s in seeds:
            try:
                rec = mine(s, wd)
            except Exception as e:
                rec = {'advisory': s.adv_id, 'error': str(e)[:200]}
            total_fns += len(rec.get('vuln_functions') or [])
            print(json.dumps(rec))
    print(f"# seeds={len(seeds)} vuln_functions_total={total_fns}", file=sys.stderr)


if __name__ == '__main__':
    main()
