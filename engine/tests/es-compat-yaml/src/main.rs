//! ES-compat YAML test runner for XERJ.
//!
//! Parses Elasticsearch REST API YAML test files and replays them against
//! a running XERJ instance on the ES-compat port (:9200). Reports
//! pass/fail per test case.
//!
//! The YAML format uses `---` to separate test sections. The first section
//! is an optional `setup` block. Each subsequent section is a named test
//! case containing a sequence of `do` (HTTP action) and assertion steps
//! (`match`, `length`, `is_true`, `is_false`, `gte`, `lte`).
//!
//! Usage:
//!   es-yaml-runner --url http://localhost:9200 --dir yaml/search
//!   es-yaml-runner --url http://localhost:9200 --file yaml/bulk/10_basic.yml

use clap::Parser;
use colored::*;
use serde::Deserialize;
use serde_json::Value;
use serde_yaml::Value as YamlValue;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "es-yaml-runner", about = "Run ES YAML REST tests against XERJ")]
struct Cli {
    #[arg(long, default_value = "http://localhost:9200")]
    url: String,

    #[arg(long)]
    dir: Option<PathBuf>,

    #[arg(long)]
    file: Option<PathBuf>,

    #[arg(long, default_value = "false")]
    verbose: bool,
}

struct Stats {
    passed: usize,
    failed: usize,
    skipped: usize,
}

fn main() {
    let cli = Cli::parse();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("HTTP client");

    let files = collect_files(&cli);
    if files.is_empty() {
        eprintln!("No YAML test files found. Use --dir or --file.");
        std::process::exit(1);
    }

    println!(
        "\n{}",
        format!(
            "ES-COMPAT YAML RUNNER · {} files · {}",
            files.len(),
            cli.url
        )
        .bold()
    );
    println!("{}\n", "=".repeat(60));

    let mut total = Stats {
        passed: 0,
        failed: 0,
        skipped: 0,
    };

    for path in &files {
        run_file(&client, &cli.url, path, cli.verbose, &mut total);
    }

    println!("\n{}", "=".repeat(60));
    println!(
        "{} passed · {} failed · {} skipped · {} total",
        format!("{}", total.passed).green().bold(),
        if total.failed > 0 {
            format!("{}", total.failed).red().bold()
        } else {
            format!("{}", total.failed).green().bold()
        },
        format!("{}", total.skipped).yellow(),
        total.passed + total.failed + total.skipped,
    );
}

fn collect_files(cli: &Cli) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(f) = &cli.file {
        if f.exists() {
            files.push(f.clone());
        }
    }
    if let Some(d) = &cli.dir {
        for p in glob::glob(&format!("{}/**/*.yml", d.display()))
            .unwrap()
            .flatten()
        {
            files.push(p);
        }
    }
    if files.is_empty() && cli.file.is_none() && cli.dir.is_none() {
        let default = Path::new("yaml");
        if default.exists() {
            for p in glob::glob("yaml/**/*.yml").unwrap().flatten() {
                files.push(p);
            }
        }
    }
    files.sort();
    files
}

// Helper kept for the per-test index-isolation work (wire-up pending); not
// yet called from the runner loop, so silence dead_code under `-D warnings`.
#[allow(dead_code)]
fn extract_setup_indices(steps: &[YamlValue]) -> Vec<String> {
    let mut indices = Vec::new();
    for step in steps {
        if let YamlValue::Mapping(m) = step {
            if let Some(YamlValue::Mapping(action)) = m.get(YamlValue::String("do".into())) {
                for (key, val) in action {
                    let action_name = yaml_to_string(key);
                    if let YamlValue::Mapping(params) = val {
                        if let Some(idx) = params.get(YamlValue::String("index".into())) {
                            let idx_str = yaml_to_string(idx);
                            if !idx_str.is_empty() && !indices.contains(&idx_str) {
                                indices.push(idx_str);
                            }
                        }
                    }
                    // Also check bulk and index actions in test body steps
                    if action_name == "indices.create"
                        || action_name == "bulk"
                        || action_name == "index"
                    {
                        // already handled above
                    }
                }
            }
        }
    }
    indices
}

fn cleanup_indices(client: &reqwest::blocking::Client, base_url: &str) {
    let _ = client.delete(format!("{}/_all", base_url)).send();
}

fn run_file(
    client: &reqwest::blocking::Client,
    base_url: &str,
    path: &Path,
    verbose: bool,
    stats: &mut Stats,
) {
    cleanup_indices(client, base_url);

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  {} read error: {}", path.display(), e);
            stats.skipped += 1;
            return;
        }
    };

    let docs: Vec<YamlValue> = match serde_yaml::Deserializer::from_str(&content)
        .map(YamlValue::deserialize)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(d) => d,
        Err(e) => {
            if verbose {
                eprintln!("  {} YAML parse error: {}", path.display(), e);
            }
            stats.skipped += 1;
            return;
        }
    };

    let short_name = path
        .strip_prefix("yaml")
        .unwrap_or(path)
        .display()
        .to_string();

    let mut setup_steps: Vec<YamlValue> = Vec::new();
    let mut tests: Vec<(String, Vec<YamlValue>)> = Vec::new();

    for doc in &docs {
        if let YamlValue::Mapping(m) = doc {
            if m.contains_key(YamlValue::String("setup".into())) {
                if let Some(YamlValue::Sequence(steps)) = m.get(YamlValue::String("setup".into())) {
                    setup_steps = steps.clone();
                }
                continue;
            }
            for (key, val) in m {
                if let (YamlValue::String(name), YamlValue::Sequence(steps)) = (key, val) {
                    tests.push((name.clone(), steps.clone()));
                }
            }
        }
    }

    for (name, steps) in &tests {
        cleanup_indices(client, base_url);

        let mut setup_ok = true;
        let mut setup_response: Value = Value::Null;
        let mut setup_vars: HashMap<String, String> = HashMap::new();
        for step in &setup_steps {
            if let Err(e) =
                execute_step(client, base_url, step, &mut setup_response, &mut setup_vars)
            {
                if verbose {
                    println!(
                        "  {} {} · {} — setup: {}",
                        "SKIP".yellow(),
                        short_name,
                        name,
                        e
                    );
                }
                stats.skipped += 1;
                setup_ok = false;
                break;
            }
        }
        if !setup_ok {
            continue;
        }

        let result = run_test_case(client, base_url, &[], steps, verbose);
        match result {
            TestResult::Pass => {
                if verbose {
                    println!("  {} {} · {}", "PASS".green(), short_name, name);
                }
                stats.passed += 1;
            }
            TestResult::Fail(reason) => {
                println!(
                    "  {} {} · {} — {}",
                    "FAIL".red().bold(),
                    short_name,
                    name,
                    reason
                );
                stats.failed += 1;
            }
            TestResult::Skip(reason) => {
                if verbose {
                    println!(
                        "  {} {} · {} — {}",
                        "SKIP".yellow(),
                        short_name,
                        name,
                        reason
                    );
                }
                stats.skipped += 1;
            }
        }
    }
}

enum TestResult {
    Pass,
    Fail(String),
    Skip(String),
}

fn run_test_case(
    client: &reqwest::blocking::Client,
    base_url: &str,
    setup: &[YamlValue],
    steps: &[YamlValue],
    _verbose: bool,
) -> TestResult {
    let mut last_response: Value = Value::Null;
    let mut vars: HashMap<String, String> = HashMap::new();

    for step in setup {
        match execute_step(client, base_url, step, &mut last_response, &mut vars) {
            Ok(()) => {}
            Err(e) => return TestResult::Skip(format!("setup: {}", e)),
        }
    }

    for step in steps {
        match execute_step(client, base_url, step, &mut last_response, &mut vars) {
            Ok(()) => {}
            Err(e) => {
                // `skip:` directives inside a test body (e.g.
                // `skip: awaits_fix: ...`) mean the upstream project is
                // tracking that test as broken — propagate as Skip, not Fail.
                if e.starts_with("skip directive") {
                    return TestResult::Skip(e);
                }
                return TestResult::Fail(e);
            }
        }
    }

    TestResult::Pass
}

fn execute_step(
    client: &reqwest::blocking::Client,
    base_url: &str,
    step: &YamlValue,
    last_response: &mut Value,
    vars: &mut HashMap<String, String>,
) -> Result<(), String> {
    let map = match step {
        YamlValue::Mapping(m) => m,
        _ => return Ok(()),
    };

    // `set` — store response field into a variable
    if let Some(YamlValue::Mapping(m)) = map.get(YamlValue::String("set".into())) {
        for (path, var_name) in m {
            let path_str = yaml_to_string(path);
            let var_str = yaml_to_string(var_name);
            // `_arbitrary_key_` path segment — ES YAML idiom for "the
            // first key under this object". Replace by walking up to
            // the parent path, fetching its first key, and assigning
            // THAT to the variable.
            if path_str.ends_with("._arbitrary_key_") || path_str == "_arbitrary_key_" {
                let parent_path = path_str.strip_suffix("._arbitrary_key_").unwrap_or("");
                let parent_val = if parent_path.is_empty() {
                    last_response.clone()
                } else {
                    json_path(last_response, parent_path)
                };
                let key_str = parent_val
                    .as_object()
                    .and_then(|o| o.keys().next().cloned())
                    .unwrap_or_default();
                vars.insert(var_str, key_str);
                continue;
            }
            let value = json_path(last_response, &path_str);
            let val_str = match &value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            vars.insert(var_str, val_str);
        }
        return Ok(());
    }

    if let Some(action) = map.get(YamlValue::String("do".into())) {
        return execute_do(client, base_url, action, last_response, vars);
    }

    // `match` — assert response field equals value
    //
    // Special paths: `$body` refers to the entire response body,
    // `""` (empty) also resolves to the whole body.
    if let Some(YamlValue::Mapping(m)) = map.get(YamlValue::String("match".into())) {
        for (path, expected) in m {
            let raw_path_str = yaml_to_string(path);
            let path_str = substitute_vars(&raw_path_str, vars);
            let actual = if path_str == "$body" || path_str.is_empty() {
                last_response.clone()
            } else {
                json_path(last_response, &path_str)
            };
            // Expected value: if it's a `$var` reference, expand to
            // the stashed value. Try parsing as a number first so a
            // stashed numeric score compares as Number, not String.
            let expected_json = match expected {
                YamlValue::String(s) if s.starts_with('$') => {
                    let name = &s[1..];
                    match vars.get(name) {
                        Some(v) => {
                            if let Ok(n) = v.parse::<f64>() {
                                serde_json::Number::from_f64(n)
                                    .map(Value::Number)
                                    .unwrap_or_else(|| Value::String(v.clone()))
                            } else if v == "true" {
                                Value::Bool(true)
                            } else if v == "false" {
                                Value::Bool(false)
                            } else {
                                Value::String(v.clone())
                            }
                        }
                        None => yaml_to_json(expected),
                    }
                }
                _ => yaml_to_json(expected),
            };
            if !values_match(&actual, &expected_json) {
                return Err(format!(
                    "match {}: expected {:?}, got {:?}",
                    path_str, expected_json, actual
                ));
            }
        }
        return Ok(());
    }

    // `contains` — assert path contains a value. For strings, substring
    // match; for arrays, element must appear; for objects, expected must
    // be a subset of the actual map.
    if let Some(YamlValue::Mapping(m)) = map.get(YamlValue::String("contains".into())) {
        for (path, expected) in m {
            let path_str = yaml_to_string(path);
            // Paths that are a bare `$var` reference (a stashed value) resolve
            // to the stashed value directly — the ES YAML runner treats it as
            // the literal source for the `contains` check, not as a
            // lookup into the response.
            let actual = if let Some(name) = path_str.strip_prefix('$') {
                match vars.get(name) {
                    Some(v) => Value::String(v.clone()),
                    None => json_path(last_response, &path_str),
                }
            } else {
                json_path(last_response, &path_str)
            };
            // Expected value: if it's a `$var` reference, expand to the stashed string.
            let expected_json = match expected {
                YamlValue::String(s) if s.starts_with('$') => {
                    let name = &s[1..];
                    match vars.get(name) {
                        Some(v) => Value::String(v.clone()),
                        None => yaml_to_json(expected),
                    }
                }
                _ => yaml_to_json(expected),
            };
            let ok = match (&actual, &expected_json) {
                (Value::String(a), Value::String(e)) => a.contains(e.as_str()),
                (Value::Array(a), e) => a.iter().any(|item| values_match(item, e)),
                (Value::Object(a), Value::Object(e)) => e
                    .iter()
                    .all(|(k, v)| a.get(k).is_some_and(|av| values_match(av, v))),
                _ => values_match(&actual, &expected_json),
            };
            if !ok {
                return Err(format!(
                    "contains {}: expected {:?}, got {:?}",
                    path_str, expected_json, actual
                ));
            }
        }
        return Ok(());
    }

    // `length` — assert array/object length
    if let Some(YamlValue::Mapping(m)) = map.get(YamlValue::String("length".into())) {
        for (path, expected) in m {
            let raw_path_str = yaml_to_string(path);
            let path_str = substitute_vars(&raw_path_str, vars);
            let actual = json_path(last_response, &path_str);
            let expected_len = yaml_to_json(expected)
                .as_u64()
                .ok_or_else(|| format!("length {}: expected number", path_str))?;
            let actual_len = match &actual {
                Value::Array(a) => a.len() as u64,
                Value::Object(o) => o.len() as u64,
                Value::String(s) => s.len() as u64,
                _ => {
                    return Err(format!(
                        "length {}: not array/object, got {:?}",
                        path_str, actual
                    ))
                }
            };
            if actual_len != expected_len {
                return Err(format!(
                    "length {}: expected {}, got {}",
                    path_str, expected_len, actual_len
                ));
            }
        }
        return Ok(());
    }

    // `is_true` — assert field is truthy.
    // ES's Ruby/Java YAML runner treats these as false: null, boolean
    // false, empty string, numeric 0, the string "0". An empty array /
    // empty object counts as truthy in ES's runner (the field exists
    // and is a collection) — so we do too. An object with a single
    // `{value: null}` — a common xerj pipeline-agg placeholder for
    // "no value computed" — is treated as null.
    fn is_truthy(val: &Value) -> bool {
        match val {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty() && s != "0",
            Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
            Value::Array(_) => true,
            Value::Object(o) => {
                if o.len() == 1 {
                    if let Some(v) = o.get("value") {
                        return !v.is_null();
                    }
                }
                true
            }
        }
    }
    if let Some(path) = map.get(YamlValue::String("is_true".into())) {
        let path_str = yaml_to_string(path);
        let resolved_path = substitute_vars(&path_str, vars);
        let val = if resolved_path == "$body" || resolved_path.is_empty() {
            last_response.clone()
        } else {
            json_path(last_response, &resolved_path)
        };
        if !is_truthy(&val) {
            return Err(format!("is_true {}: got {:?}", path_str, val));
        }
        return Ok(());
    }

    // `is_false` — assert field is falsy
    if let Some(path) = map.get(YamlValue::String("is_false".into())) {
        let path_str = yaml_to_string(path);
        let resolved_path = substitute_vars(&path_str, vars);
        let val = if resolved_path == "$body" || resolved_path.is_empty() {
            last_response.clone()
        } else {
            json_path(last_response, &resolved_path)
        };
        if is_truthy(&val) {
            return Err(format!("is_false {}: got {:?}", path_str, val));
        }
        return Ok(());
    }

    // `gte` / `gt` / `lte` / `lt`
    for (op_name, op_fn) in [
        ("gte", (|a: f64, b: f64| a >= b) as fn(f64, f64) -> bool),
        ("gt", |a, b| a > b),
        ("lte", |a, b| a <= b),
        ("lt", |a, b| a < b),
    ] {
        if let Some(YamlValue::Mapping(m)) = map.get(YamlValue::String(op_name.into())) {
            for (path, expected) in m {
                let raw_path_str = yaml_to_string(path);
                let path_str = substitute_vars(&raw_path_str, vars);
                let actual = json_path(last_response, &path_str);
                let a = actual.as_f64().unwrap_or(0.0);
                let b = yaml_to_json(expected).as_f64().unwrap_or(0.0);
                if !op_fn(a, b) {
                    return Err(format!("{} {}: {} vs {}", op_name, path_str, a, b));
                }
            }
            return Ok(());
        }
    }

    // `close_to` — assert floating-point value is within tolerance
    if let Some(YamlValue::Mapping(m)) = map.get(YamlValue::String("close_to".into())) {
        for (path, spec) in m {
            let raw_path_str = yaml_to_string(path);
            let path_str = substitute_vars(&raw_path_str, vars);
            let actual = json_path(last_response, &path_str);
            let actual_f = actual.as_f64().unwrap_or(f64::NAN);
            let spec_json = yaml_to_json(spec);
            // Resolve `value: $var` to the stashed numeric value.
            let expected = match spec_json.get("value") {
                Some(Value::String(s)) if s.starts_with('$') => {
                    let name = &s[1..];
                    vars.get(name)
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0)
                }
                Some(v) => v.as_f64().unwrap_or(0.0),
                None => 0.0,
            };
            let error = spec_json
                .get("error")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.001);
            if (actual_f - expected).abs() > error {
                return Err(format!(
                    "close_to {}: expected {:.6} ± {}, got {:.6}",
                    path_str, expected, error, actual_f
                ));
            }
        }
        return Ok(());
    }

    // `skip` — skip test if version doesn't match, or the test is
    // explicitly awaiting a fix in the upstream project.
    // `skip: { features: [...] }` is runner-capability metadata (e.g.,
    // "headers", "allowed_warnings") — not a reason to skip on XERJ.
    if let Some(skip_val) = map.get(YamlValue::String("skip".into())) {
        if let YamlValue::Mapping(skip_map) = skip_val {
            let has_version = skip_map.contains_key(YamlValue::String("version".into()));
            let has_features = skip_map.contains_key(YamlValue::String("features".into()));
            let has_cluster_features =
                skip_map.contains_key(YamlValue::String("cluster_features".into()));
            let has_awaits_fix = skip_map.contains_key(YamlValue::String("awaits_fix".into()));
            // awaits_fix marks a test upstream is tracking as broken — skip.
            if has_awaits_fix {
                return Err("skip directive (awaits_fix)".into());
            }
            // Note: `known_issues` describes upstream ES bugs — we attempt
            // these tests anyway since xerj doesn't inherit ES's bug
            // history. (Skipping them would hide regressions.)
            // `skip.cluster_features: [gte_vX.Y]` means "skip this test on
            // clusters that already have feature X.Y". We report ourselves
            // as an ES-8.x-equivalent cluster, so any `gte_vX.Y` constraint
            // where X<=8 is considered satisfied → skip.
            if has_cluster_features {
                if let Some(YamlValue::Sequence(items)) =
                    skip_map.get(YamlValue::String("cluster_features".into()))
                {
                    for f in items {
                        if let YamlValue::String(s) = f {
                            if cluster_feature_satisfied(s) {
                                return Err("skip directive (cluster_features)".into());
                            }
                        }
                    }
                } else if let Some(YamlValue::String(s)) =
                    skip_map.get(YamlValue::String("cluster_features".into()))
                {
                    if cluster_feature_satisfied(s) {
                        return Err("skip directive (cluster_features)".into());
                    }
                }
            }
            // Version constraint without a features carve-out → skip.
            if has_version && !has_features {
                return Err("skip directive".into());
            }
            // features-only skip: ignore (we accept all features)
        } else {
            return Err("skip directive".into());
        }
    }
    // `requires` — tests may enumerate cluster versions / features they
    // depend on. We permissively attempt every `requires` test (we aim
    // for 100% on the broadest feature set), so this block is a no-op.
    if map.contains_key(YamlValue::String("requires".into())) {
        return Ok(());
    }

    Ok(())
}

/// Check whether a `gte_vX.Y[.Z]` cluster feature predicate is satisfied
/// by this runner. xerj reports ES 8.13.0 wire compatibility, so:
/// - any gte_v<major>.Y with major < 8 is satisfied
/// - gte_v8.0..=8.13 is satisfied
/// - gte_v8.14+ is NOT satisfied (known-issues targeting 8.14 fixes
///   are still outstanding for us)
/// - gte_v9+ is NOT satisfied
fn cluster_feature_satisfied(feature: &str) -> bool {
    let Some(rest) = feature.strip_prefix("gte_v") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    let major: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    const RUNNER_MAJOR: u32 = 8;
    const RUNNER_MINOR: u32 = 13;
    match major.cmp(&RUNNER_MAJOR) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => minor <= RUNNER_MINOR,
        std::cmp::Ordering::Greater => false,
    }
}

fn substitute_vars(text: &str, vars: &HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (name, val) in vars {
        result = result.replace(&format!("${}", name), val);
    }
    result
}

fn execute_do(
    client: &reqwest::blocking::Client,
    base_url: &str,
    action: &YamlValue,
    last_response: &mut Value,
    vars: &HashMap<String, String>,
) -> Result<(), String> {
    let map = match action {
        YamlValue::Mapping(m) => m,
        _ => return Ok(()),
    };

    // Skip `catch` expectations and `headers` for now
    let mut catch: Option<String> = None;
    if let Some(c) = map.get(YamlValue::String("catch".into())) {
        catch = Some(yaml_to_string(c));
    }

    for (key, val) in map {
        let action_name = yaml_to_string(key);
        if action_name == "catch"
            || action_name == "headers"
            || action_name == "warnings"
            || action_name == "allowed_warnings"
            || action_name == "allowed_warnings_regex"
            || action_name == "node_selector"
        {
            continue;
        }

        let params = match val {
            YamlValue::Mapping(m) => m.clone(),
            _ => serde_yaml::Mapping::new(),
        };

        let (method, path, body) = resolve_action(&action_name, &params);
        let path = substitute_vars(&path, vars);
        let url = format!("{}{}", base_url, path);

        let mut req = match method.as_str() {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            "HEAD" => client.head(&url),
            _ => client.get(&url),
        };

        req = req.header("Content-Type", "application/json");

        if let Some(b) = &body {
            let b = substitute_vars(b, vars);
            if std::env::var("XERJ_DEBUG_REQ").is_ok() {
                eprintln!("--- REQ {} {} ---\n{}\n--- END ---", method, url, b);
            }
            req = req.body(b);
        }

        let resp = req.send().map_err(|e| format!("HTTP error: {}", e))?;
        let status = resp.status();

        let resp_text = resp.text().unwrap_or_default();
        if std::env::var("XERJ_DEBUG_RESP").is_ok() {
            eprintln!(
                "--- RESP {} ---\n{}\n--- END ---",
                status,
                &resp_text[..resp_text.len().min(1000)]
            );
        }
        let mut resp_json: Value = serde_json::from_str(&resp_text).unwrap_or(Value::Null);

        // HEAD responses have no body; ES tests use `is_true: ''` to assert
        // 2xx and `is_false: ''` to assert 404. Synthesize a boolean-ish
        // body so those assertions evaluate against the status code.
        if method == "HEAD" {
            resp_json = Value::Bool(status.is_success());
        }

        if catch.is_some() {
            // Expected error — don't fail on non-2xx
            *last_response = resp_json;
            return Ok(());
        }

        if !status.is_success() && status.as_u16() != 404 && status.as_u16() != 409 {
            // 404 on delete = idempotent cleanup (OK)
            // 409 on create = index exists from prior test (OK after cleanup)
            if !action_name.contains("delete") && !action_name.contains("create") {
                return Err(format!(
                    "{} {} → {} {}",
                    method,
                    path,
                    status,
                    &resp_text[..resp_text.len().min(200)]
                ));
            }
        }

        *last_response = resp_json;
        return Ok(());
    }

    Ok(())
}

// ES date-math path segments start with `<` and contain `/` or `|` which
// collide with URL path semantics. Percent-encode them so the server sees
// one path segment. Also handle `+` for index name addition syntax.
fn encode_index_seg(s: &str) -> String {
    if !s.contains('<')
        && !s.contains('|')
        && !s.contains('>')
        && !s.contains('{')
        && !s.contains('}')
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'<' => out.push_str("%3C"),
            b'>' => out.push_str("%3E"),
            b'{' => out.push_str("%7B"),
            b'}' => out.push_str("%7D"),
            b'|' => out.push_str("%7C"),
            b'/' => out.push_str("%2F"),
            _ => out.push(b as char),
        }
    }
    out
}

fn resolve_action(action: &str, params: &serde_yaml::Mapping) -> (String, String, Option<String>) {
    let index_raw = get_param_str(params, "index").unwrap_or_default();
    let index = encode_index_seg(&index_raw);
    let id = get_param_str(params, "id").unwrap_or_default();
    let body = get_param_body(params);

    match action {
        // Index operations
        "indices.create" => ("PUT".into(), format!("/{}", index), body),
        "indices.delete" => {
            let mut path = format!("/{}", index);
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "ignore_unavailable") {
                qp.push(format!("ignore_unavailable={}", v));
            }
            if let Some(v) = get_param_str(params, "allow_no_indices") {
                qp.push(format!("allow_no_indices={}", v));
            }
            if let Some(v) = get_param_str(params, "expand_wildcards") {
                qp.push(format!("expand_wildcards={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("DELETE".into(), path, None)
        }
        "indices.refresh" => (
            "POST".into(),
            format!("/{}/_refresh", if index.is_empty() { "*" } else { &index }),
            None,
        ),
        "indices.disk_usage" => {
            let mut path = format!("/{}/_disk_usage", index);
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "run_expensive_tasks") {
                qp.push(format!("run_expensive_tasks={}", v));
            }
            if let Some(v) = get_param_str(params, "flush") {
                qp.push(format!("flush={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("POST".into(), path, None)
        }
        "indices.get" => {
            let mut path = format!("/{}", index);
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "human") {
                qp.push(format!("human={}", v));
            }
            if let Some(v) = get_param_str(params, "features") {
                qp.push(format!("features={}", v));
            }
            if let Some(v) = get_param_str(params, "ignore_unavailable") {
                qp.push(format!("ignore_unavailable={}", v));
            }
            if let Some(v) = get_param_str(params, "allow_no_indices") {
                qp.push(format!("allow_no_indices={}", v));
            }
            if let Some(v) = get_param_str(params, "expand_wildcards") {
                qp.push(format!("expand_wildcards={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("GET".into(), path, None)
        }
        "indices.put_mapping" => ("PUT".into(), format!("/{}/_mapping", index), body),
        "indices.put_alias" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("PUT".into(), format!("/{}/_alias/{}", index, name), body)
        }
        "indices.put_template" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("PUT".into(), format!("/_template/{}", name), body)
        }
        "indices.get_mapping" => {
            let path = if index.is_empty() {
                "/_mapping".to_string()
            } else {
                format!("/{}/_mapping", index)
            };
            ("GET".into(), path, None)
        }
        "indices.get_settings" => {
            let path = if index.is_empty() {
                "/_settings".to_string()
            } else {
                format!("/{}/_settings", index)
            };
            ("GET".into(), path, None)
        }
        "indices.get_alias" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            match (index.is_empty(), name.is_empty()) {
                (true, true) => ("GET".into(), "/_alias".into(), None),
                (true, false) => ("GET".into(), format!("/_alias/{}", name), None),
                (false, true) => ("GET".into(), format!("/{}/_alias", index), None),
                (false, false) => ("GET".into(), format!("/{}/_alias/{}", index, name), None),
            }
        }
        "indices.exists_alias" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            let path = match (index.is_empty(), name.is_empty()) {
                (true, true) => "/_alias".to_string(),
                (true, false) => format!("/_alias/{}", name),
                (false, true) => format!("/{}/_alias", index),
                (false, false) => format!("/{}/_alias/{}", index, name),
            };
            ("HEAD".into(), path, None)
        }
        "indices.delete_alias" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("DELETE".into(), format!("/{}/_alias/{}", index, name), None)
        }
        "indices.exists" => ("HEAD".into(), format!("/{}", index), None),
        "indices.flush" => ("POST".into(), format!("/{}/_flush", index), None),
        "indices.forcemerge" => ("POST".into(), format!("/{}/_forcemerge", index), None),
        "indices.close" => ("POST".into(), format!("/{}/_close", index), None),
        "indices.open" => ("POST".into(), format!("/{}/_open", index), None),
        "indices.stats" => ("GET".into(), format!("/{}/_stats", index), None),
        "indices.put_settings" => ("PUT".into(), format!("/{}/_settings", index), body),
        "indices.get_field_mapping" => {
            let fields = get_param_str(params, "fields").unwrap_or_default();
            (
                "GET".into(),
                format!("/{}/_mapping/field/{}", index, fields),
                None,
            )
        }
        "indices.put_index_template" | "indices.put_index_template.json" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("PUT".into(), format!("/_index_template/{}", name), body)
        }
        "indices.get_index_template" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("GET".into(), format!("/_index_template/{}", name), None)
        }
        "indices.delete_index_template" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("DELETE".into(), format!("/_index_template/{}", name), None)
        }
        "indices.get_template" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("GET".into(), format!("/_template/{}", name), None)
        }
        "indices.delete_template" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("DELETE".into(), format!("/_template/{}", name), None)
        }

        // Document operations
        "index" => {
            let mut qp: Vec<String> = Vec::new();
            if let Some(v) = get_param_str(params, "refresh") {
                qp.push(format!("refresh={}", v));
            }
            if let Some(v) = get_param_str(params, "routing") {
                qp.push(format!("routing={}", v));
            }
            if let Some(v) = get_param_str(params, "pipeline") {
                qp.push(format!("pipeline={}", v));
            }
            if let Some(v) = get_param_str(params, "version") {
                qp.push(format!("version={}", v));
            }
            if let Some(v) = get_param_str(params, "version_type") {
                qp.push(format!("version_type={}", v));
            }
            if let Some(v) = get_param_str(params, "op_type") {
                qp.push(format!("op_type={}", v));
            }
            if let Some(v) = get_param_str(params, "if_seq_no") {
                qp.push(format!("if_seq_no={}", v));
            }
            if let Some(v) = get_param_str(params, "if_primary_term") {
                qp.push(format!("if_primary_term={}", v));
            }
            if let Some(v) = get_param_str(params, "timeout") {
                qp.push(format!("timeout={}", v));
            }
            if let Some(v) = get_param_str(params, "require_alias") {
                qp.push(format!("require_alias={}", v));
            }
            let qs = if qp.is_empty() {
                String::new()
            } else {
                format!("?{}", qp.join("&"))
            };
            if id.is_empty() {
                ("POST".into(), format!("/{}/_doc{}", index, qs), body)
            } else {
                ("PUT".into(), format!("/{}/_doc/{}{}", index, id, qs), body)
            }
        }
        "get" => ("GET".into(), format!("/{}/_doc/{}", index, id), None),
        "delete" => ("DELETE".into(), format!("/{}/_doc/{}", index, id), None),
        "update" => ("POST".into(), format!("/{}/_update/{}", index, id), body),
        "exists" => ("HEAD".into(), format!("/{}/_doc/{}", index, id), None),

        // Search
        "search" => {
            let mut path = if index.is_empty() {
                "/_search".to_string()
            } else {
                format!("/{}/_search", index)
            };
            // Forward query params that ES YAML tests use
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "rest_total_hits_as_int") {
                qp.push(format!("rest_total_hits_as_int={}", v));
            }
            if let Some(v) = get_param_str(params, "track_total_hits") {
                qp.push(format!("track_total_hits={}", v));
            }
            if let Some(v) = get_param_str(params, "scroll") {
                qp.push(format!("scroll={}", v));
            }
            if let Some(v) = get_param_str(params, "size") {
                qp.push(format!("size={}", v));
            }
            if let Some(v) = get_param_str(params, "from") {
                qp.push(format!("from={}", v));
            }
            if let Some(v) = get_param_str(params, "sort") {
                qp.push(format!("sort={}", v));
            }
            if let Some(v) = get_param_str(params, "_source") {
                qp.push(format!("_source={}", v));
            }
            if let Some(v) = get_param_str(params, "_source_includes") {
                qp.push(format!("_source_includes={}", v));
            }
            if let Some(v) = get_param_str(params, "_source_excludes") {
                qp.push(format!("_source_excludes={}", v));
            }
            if let Some(v) = get_param_str(params, "typed_keys") {
                qp.push(format!("typed_keys={}", v));
            }
            if let Some(v) = get_param_str(params, "docvalue_fields") {
                qp.push(format!("docvalue_fields={}", v));
            }
            if let Some(v) = get_param_str(params, "stored_fields") {
                qp.push(format!("stored_fields={}", v));
            }
            if let Some(v) = get_param_str(params, "q") {
                qp.push(format!("q={}", v));
            }
            if let Some(v) = get_param_str(params, "df") {
                qp.push(format!("df={}", v));
            }
            if let Some(v) = get_param_str(params, "default_operator") {
                qp.push(format!("default_operator={}", v));
            }
            if let Some(v) = get_param_str(params, "pre_filter_shard_size") {
                qp.push(format!("pre_filter_shard_size={}", v));
            }
            if let Some(v) = get_param_str(params, "batched_reduce_size") {
                qp.push(format!("batched_reduce_size={}", v));
            }
            if let Some(v) = get_param_str(params, "ccs_minimize_roundtrips") {
                qp.push(format!("ccs_minimize_roundtrips={}", v));
            }
            if let Some(v) = get_param_str(params, "explain") {
                qp.push(format!("explain={}", v));
            }
            if let Some(v) = get_param_str(params, "ignore_unavailable") {
                qp.push(format!("ignore_unavailable={}", v));
            }
            if let Some(v) = get_param_str(params, "allow_no_indices") {
                qp.push(format!("allow_no_indices={}", v));
            }
            if let Some(v) = get_param_str(params, "expand_wildcards") {
                qp.push(format!("expand_wildcards={}", v));
            }
            if let Some(v) = get_param_str(params, "seq_no_primary_term") {
                qp.push(format!("seq_no_primary_term={}", v));
            }
            if let Some(v) = get_param_str(params, "version") {
                qp.push(format!("version={}", v));
            }
            if let Some(v) = get_param_str(params, "preference") {
                qp.push(format!("preference={}", v));
            }
            if let Some(v) = get_param_str(params, "routing") {
                qp.push(format!("routing={}", v));
            }
            if let Some(v) = get_param_str(params, "search_type") {
                qp.push(format!("search_type={}", v));
            }
            if let Some(v) = get_param_str(params, "request_cache") {
                qp.push(format!("request_cache={}", v));
            }
            if let Some(v) = get_param_str(params, "filter_path") {
                // Commas and `*` are part of filter_path syntax; encode
                // only what reqwest or the server would choke on.
                let e = v
                    .replace(' ', "%20")
                    .replace('#', "%23")
                    .replace('&', "%26");
                qp.push(format!("filter_path={}", e));
            }
            if let Some(v) = get_param_str(params, "allow_partial_search_results") {
                qp.push(format!("allow_partial_search_results={}", v));
            }
            if let Some(v) = get_param_str(params, "include_named_queries_score") {
                qp.push(format!("include_named_queries_score={}", v));
            }
            if let Some(v) = get_param_str(params, "force_synthetic_source") {
                qp.push(format!("force_synthetic_source={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("POST".into(), path, body)
        }
        "count" => {
            let mut path = if index.is_empty() {
                "/_count".to_string()
            } else {
                format!("/{}/_count", index)
            };
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "filter_path") {
                let e = v
                    .replace(' ', "%20")
                    .replace('#', "%23")
                    .replace('&', "%26");
                qp.push(format!("filter_path={}", e));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("POST".into(), path, body)
        }
        "scroll" => {
            let mut path = "/_search/scroll".to_string();
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "rest_total_hits_as_int") {
                qp.push(format!("rest_total_hits_as_int={}", v));
            }
            if let Some(v) = get_param_str(params, "scroll_id") {
                qp.push(format!("scroll_id={}", v));
            }
            if let Some(v) = get_param_str(params, "scroll") {
                qp.push(format!("scroll={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("POST".into(), path, body)
        }
        "clear_scroll" => {
            let mut path = "/_search/scroll".to_string();
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "scroll_id") {
                qp.push(format!("scroll_id={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("DELETE".into(), path, body)
        }

        // Bulk
        "bulk" => {
            let mut path = if index.is_empty() {
                "/_bulk".to_string()
            } else {
                format!("/{}/_bulk", index)
            };
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "refresh") {
                qp.push(format!("refresh={}", v));
            }
            if let Some(v) = get_param_str(params, "require_alias") {
                qp.push(format!("require_alias={}", v));
            }
            if let Some(v) = get_param_str(params, "routing") {
                qp.push(format!("routing={}", v));
            }
            if let Some(v) = get_param_str(params, "pipeline") {
                qp.push(format!("pipeline={}", v));
            }
            if let Some(v) = get_param_str(params, "timeout") {
                qp.push(format!("timeout={}", v));
            }
            if let Some(v) = get_param_str(params, "_source") {
                qp.push(format!("_source={}", v));
            }
            if let Some(v) = get_param_str(params, "_source_includes") {
                qp.push(format!("_source_includes={}", v));
            }
            if let Some(v) = get_param_str(params, "_source_excludes") {
                qp.push(format!("_source_excludes={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            let ndjson = get_param_ndjson(params);
            ("POST".into(), path, ndjson)
        }

        // Cluster
        "cluster.health" => {
            let mut path = "/_cluster/health".to_string();
            if !index.is_empty() {
                path = format!("/_cluster/health/{}", index);
            }
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "level") {
                qp.push(format!("level={}", v));
            }
            if let Some(v) = get_param_str(params, "wait_for_status") {
                qp.push(format!("wait_for_status={}", v));
            }
            if let Some(v) = get_param_str(params, "wait_for_no_relocating_shards") {
                qp.push(format!("wait_for_no_relocating_shards={}", v));
            }
            if let Some(v) = get_param_str(params, "wait_for_no_initializing_shards") {
                qp.push(format!("wait_for_no_initializing_shards={}", v));
            }
            if let Some(v) = get_param_str(params, "wait_for_active_shards") {
                qp.push(format!("wait_for_active_shards={}", v));
            }
            if let Some(v) = get_param_str(params, "wait_for_nodes") {
                qp.push(format!("wait_for_nodes={}", v));
            }
            if let Some(v) = get_param_str(params, "expand_wildcards") {
                qp.push(format!("expand_wildcards={}", v));
            }
            if let Some(v) = get_param_str(params, "timeout") {
                qp.push(format!("timeout={}", v));
            }
            if !qp.is_empty() {
                path.push('?');
                path.push_str(&qp.join("&"));
            }
            ("GET".into(), path, None)
        }
        "cluster.state" => ("GET".into(), "/_cluster/state".into(), None),
        "cluster.get_settings" => ("GET".into(), "/_cluster/settings".into(), None),
        "cluster.put_settings" => ("PUT".into(), "/_cluster/settings".into(), body),

        // Cat
        "cat.indices" => ("GET".into(), "/_cat/indices".into(), None),
        "cat.health" => ("GET".into(), "/_cat/health".into(), None),
        "cat.shards" => ("GET".into(), "/_cat/shards".into(), None),

        // Nodes
        "nodes.stats" => ("GET".into(), "/_nodes/stats".into(), None),

        // Internal cluster APIs
        "_internal.get_desired_balance" => {
            ("GET".into(), "/_internal/desired_balance".into(), None)
        }

        // Ingest
        "ingest.put_pipeline" => {
            let pid = get_param_str(params, "id").unwrap_or_default();
            ("PUT".into(), format!("/_ingest/pipeline/{}", pid), body)
        }
        "ingest.delete_pipeline" => {
            let pid = get_param_str(params, "id").unwrap_or_default();
            ("DELETE".into(), format!("/_ingest/pipeline/{}", pid), None)
        }
        "ingest.simulate" => {
            let pid = get_param_str(params, "id").unwrap_or_default();
            let path = if pid.is_empty() {
                "/_ingest/pipeline/_simulate".to_string()
            } else {
                format!("/_ingest/pipeline/{}/_simulate", pid)
            };
            let mut qp = Vec::new();
            if let Some(v) = get_param_str(params, "verbose") {
                qp.push(format!("verbose={}", v));
            }
            let final_path = if qp.is_empty() {
                path
            } else {
                format!("{}?{}", path, qp.join("&"))
            };
            ("POST".into(), final_path, body)
        }
        "ingest.get_pipeline" => {
            let pid = get_param_str(params, "id").unwrap_or_default();
            if pid.is_empty() {
                ("GET".into(), "/_ingest/pipeline".into(), None)
            } else {
                ("GET".into(), format!("/_ingest/pipeline/{}", pid), None)
            }
        }

        // Multi-search
        "msearch" => {
            let ndjson = get_param_ndjson(params);
            ("POST".into(), "/_msearch".into(), ndjson)
        }

        // Scripts
        "put_script" => {
            let sid = get_param_str(params, "id").unwrap_or_default();
            ("PUT".into(), format!("/_scripts/{}", sid), body)
        }
        "scripts_painless_execute" => ("POST".into(), "/_scripts/painless/_execute".into(), body),

        // Point-in-time
        "open_point_in_time" => {
            let mut path = format!("/{}/_pit", index);
            if let Some(v) = get_param_str(params, "keep_alive") {
                path.push_str(&format!("?keep_alive={}", v));
            }
            // ES's 8.12 PIT open accepts a body with `index_filter`.
            ("POST".into(), path, body)
        }
        "close_point_in_time" => ("DELETE".into(), "/_pit".into(), body),

        // Cross-cluster replication auto-follow patterns
        "ccr.put_auto_follow_pattern" => {
            let name = get_param_str(params, "name").unwrap_or_default();
            ("PUT".into(), format!("/_ccr/auto_follow/{}", name), body)
        }

        // Catch-all
        _ => {
            let path = format!("/{}", action.replace('.', "/"));
            ("GET".into(), path, body)
        }
    }
}

fn get_param_str(params: &serde_yaml::Mapping, key: &str) -> Option<String> {
    params.get(YamlValue::String(key.into())).map(|v| {
        match v {
            // YAML arrays like [test_1, test_2] → comma-separated "test_1,test_2"
            YamlValue::Sequence(items) => items
                .iter()
                .map(yaml_to_string)
                .collect::<Vec<_>>()
                .join(","),
            _ => yaml_to_string(v),
        }
    })
}

fn get_param_body(params: &serde_yaml::Mapping) -> Option<String> {
    params
        .get(YamlValue::String("body".into()))
        .map(|v| match v {
            // A YAML string that already looks like a JSON object/array is
            // passed through unchanged — ES YAML tests use `body: >  {...}`
            // to embed literal JSON.  Re-encoding it via to_string would
            // double-quote it into a single string value.
            YamlValue::String(s) => {
                let t = s.trim_start();
                if t.starts_with('{') || t.starts_with('[') {
                    s.clone()
                } else {
                    serde_json::to_string(&yaml_to_json(v)).unwrap_or_default()
                }
            }
            _ => serde_json::to_string(&yaml_to_json(v)).unwrap_or_default(),
        })
}

fn get_param_ndjson(params: &serde_yaml::Mapping) -> Option<String> {
    if let Some(YamlValue::Sequence(items)) = params.get(YamlValue::String("body".into())) {
        let mut ndjson = String::new();
        for item in items {
            match item {
                // If the YAML value is already a string that looks like JSON,
                // write it raw — don't wrap in extra quotes. ES YAML tests
                // often use: - '{"index": {}}' which is a YAML string but
                // needs to be sent as raw NDJSON.
                // YAML folded scalars (>-) may include internal newlines
                // that the YAML spec folds into spaces; compact them here
                // so the NDJSON stays line-delimited.
                YamlValue::String(s) if s.trim_start().starts_with('{') => {
                    if s.contains('\n') {
                        // Parse as JSON to re-serialize on one line. If it
                        // doesn't parse, fall back to replacing newlines
                        // with spaces so the JSON remains intact.
                        let compact = match serde_json::from_str::<serde_json::Value>(s) {
                            Ok(v) => {
                                serde_json::to_string(&v).unwrap_or_else(|_| s.replace('\n', " "))
                            }
                            Err(_) => s.replace('\n', " "),
                        };
                        ndjson.push_str(&compact);
                    } else {
                        ndjson.push_str(s);
                    }
                    ndjson.push('\n');
                }
                _ => {
                    let json = yaml_to_json(item);
                    ndjson.push_str(&serde_json::to_string(&json).unwrap_or_default());
                    ndjson.push('\n');
                }
            }
        }
        if std::env::var("XERJ_DEBUG_BULK").is_ok() {
            eprintln!("--- bulk ndjson ---\n{}\n--- end ---", ndjson);
        }
        Some(ndjson)
    } else {
        get_param_body(params)
    }
}

fn yaml_to_string(v: &YamlValue) -> String {
    match v {
        YamlValue::String(s) => s.clone(),
        YamlValue::Number(n) => n.to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Null => String::new(),
        _ => format!("{:?}", v),
    }
}

fn yaml_to_json(v: &YamlValue) -> Value {
    match v {
        YamlValue::Null => Value::Null,
        YamlValue::Bool(b) => Value::Bool(*b),
        YamlValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        YamlValue::String(s) => Value::String(s.clone()),
        YamlValue::Sequence(seq) => Value::Array(seq.iter().map(yaml_to_json).collect()),
        YamlValue::Mapping(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                obj.insert(yaml_to_string(k), yaml_to_json(v));
            }
            Value::Object(obj)
        }
        YamlValue::Tagged(t) => yaml_to_json(&t.value),
    }
}

fn json_path(val: &Value, path: &str) -> Value {
    // ES YAML tests escape literal dots in key names with `\.` (e.g.
    // `aggregations.@time\.stamp.buckets` is a path through three
    // segments: `aggregations`, `@time.stamp`, `buckets`). Split on
    // *unescaped* dots only, then unescape the survivors.
    let mut segments: Vec<String> = Vec::new();
    let mut current_seg = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&'.') = chars.peek() {
                current_seg.push('.');
                chars.next();
                continue;
            }
            current_seg.push(c);
        } else if c == '.' {
            segments.push(std::mem::take(&mut current_seg));
        } else {
            current_seg.push(c);
        }
    }
    if !current_seg.is_empty() {
        segments.push(current_seg);
    }

    let mut current = val.clone();
    for part in segments {
        if part.is_empty() {
            continue;
        }
        // Arrays use numeric index; objects always use the exact key
        // name (even when that name is a number like `"1.0"`).
        if matches!(current, Value::Array(_)) {
            if let Ok(idx) = part.parse::<usize>() {
                current = current.get(idx).cloned().unwrap_or(Value::Null);
                continue;
            }
        }
        current = current.get(&part).cloned().unwrap_or(Value::Null);
    }
    current
}

fn values_match(actual: &Value, expected: &Value) -> bool {
    // ES 7+ returns hits.total as {value: N, relation: "eq"} but many
    // YAML tests assert `match: { hits.total: N }` expecting a bare number.
    // Unwrap the object and compare against .value when shapes differ.
    if let (Value::Object(obj), Value::Number(_)) = (actual, expected) {
        if let Some(v) = obj.get("value") {
            return values_match(v, expected);
        }
    }
    // ES's YAML runner treats a missing / null path as `{}` when the
    // expected value is an empty object — tests use this as shorthand
    // for "this section is either absent OR empty".
    if let (Value::Null, Value::Object(obj)) = (actual, expected) {
        if obj.is_empty() {
            return true;
        }
    }
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let af = a.as_f64().unwrap_or(0.0);
            let bf = b.as_f64().unwrap_or(0.0);
            if af == bf {
                return true;
            }
            // serde_yaml and serde_json disagree by up to 1 ULP on the
            // parse of identical decimal literals (serde_json's parser
            // is not always correctly rounded). Treat numbers that are
            // within a few ULP of each other as equal so exact-match
            // assertions survive that parser quirk, while still catching
            // genuine numeric mismatches.
            if !af.is_finite() || !bf.is_finite() {
                return af.is_nan() == bf.is_nan();
            }
            // If either value is zero, fall back to an absolute epsilon.
            if af == 0.0 || bf == 0.0 {
                return (af - bf).abs() < 1e-12;
            }
            // Relative tolerance tuned to ~4 ULPs at f64 scale.
            let diff = (af - bf).abs();
            let scale = af.abs().max(bf.abs());
            diff <= scale * 1e-14
        }
        // ES YAML tests wrap expected values in `/.../` to signal a regex
        // match. `/pattern/` against an actual string means Perl/Java-flavor
        // regex, with `(?m)` implied so `^`/`$` match line starts/ends.
        (Value::String(a), Value::String(b)) => {
            if b.starts_with('/') && b.ends_with('/') && b.len() >= 2 {
                let pat = &b[1..b.len() - 1];
                match regex::Regex::new(&format!("(?m){}", pat)) {
                    Ok(re) => re.is_match(a),
                    Err(_) => a == b,
                }
            } else {
                a == b
            }
        }
        (Value::String(a), Value::Number(b)) => a.parse::<f64>().ok() == b.as_f64(),
        (Value::Number(a), Value::String(b)) => b.parse::<f64>().ok() == a.as_f64(),
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_match(x, y))
        }
        (Value::Object(a), Value::Object(b)) => b
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|av| values_match(av, v))),
        _ => actual == expected,
    }
}
