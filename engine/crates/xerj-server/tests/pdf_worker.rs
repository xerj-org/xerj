use serde_json::Value;
use std::fmt::Write as _;
use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

// XERJ_PDF_WORKER_BIN is process-global and inherited by the worker child.
// cargo runs these tests on parallel threads, so one test's set_var/remove_var
// races another's run_worker spawn (and trips std's set_var concurrency abort).
// Every test that reads or writes that env var holds this lock for its body.
// Poison-tolerant: these tests assert and may panic while holding it.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn write_prose_pdf(path: &std::path::Path) {
    let prose = "Quarterly financial results show revenue increased twelve percent while operating margin improved. International customers and subscription renewals drove durable growth across the reporting period.";
    let stream = format!("BT /F1 12 Tf 72 720 Td ({prose}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        writeln!(pdf, "{} 0 obj\n{}\nendobj", index + 1, object).unwrap();
    }
    let xref = pdf.len();
    write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).unwrap();
    for offset in offsets {
        writeln!(pdf, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        pdf,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    )
    .unwrap();
    fs::write(path, pdf).unwrap();
}

fn write_mixed_text_blank_pdf(path: &std::path::Path) {
    let prose =
        "Financial statement prose appears on this page while the next page is intentionally blank.";
    let stream = format!("BT /F1 12 Tf 72 720 Td ({prose}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        writeln!(pdf, "{} 0 obj\n{}\nendobj", index + 1, object).unwrap();
    }
    let xref = pdf.len();
    write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).unwrap();
    for offset in offsets {
        writeln!(pdf, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        pdf,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    )
    .unwrap();
    fs::write(path, pdf).unwrap();
}

fn run_worker(path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("__extract-pdf")
        .arg(path)
        .output()
        .expect("start PDF worker")
}

#[test]
fn pdf_worker_is_fresh_process_deterministic_and_preserves_page_provenance() {
    let _env = env_guard();
    let temp = tempfile::tempdir().unwrap();
    let pdf = temp.path().join("quarterly-report.pdf");
    write_prose_pdf(&pdf);

    let first = run_worker(&pdf);
    let second = run_worker(&pdf);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(first.stdout, second.stdout);

    let response: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(
        response["extractor"],
        format!("xerj-autoindex/{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(response["parser"], "pdf_oxide/0.3.75");
    assert!(response["containment"]
        .as_str()
        .unwrap()
        .contains("not a security sandbox"));
    let record = &response["records"][0];
    assert_eq!(record["fields"]["page"], 1);
    assert_eq!(record["locator"], "p1-s0");
    assert!(record["fields"]["body"]
        .as_str()
        .unwrap()
        .contains("operating margin improved"));
    assert_eq!(record["fields"]["pdf_pages_total"], 1);
    assert_eq!(record["fields"]["pdf_pages_with_text"], 1);
    assert_eq!(record["fields"]["pdf_pages_omitted"], 0);
}

#[test]
fn corrupt_pdf_failure_is_actionable_and_emits_no_records() {
    let _env = env_guard();
    let temp = tempfile::tempdir().unwrap();
    let pdf = temp.path().join("corrupt.pdf");
    fs::write(&pdf, b"%PDF-1.4\nnot a PDF").unwrap();

    let output = run_worker(&pdf);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("damaged"), "{stderr}");
    assert!(stderr.contains("password"), "{stderr}");
    assert!(stderr.contains("OCR"), "{stderr}");
}

#[test]
fn mixed_text_and_empty_pages_disclose_ocr_coverage_without_dropping_good_text() {
    let _env = env_guard();
    let temp = tempfile::tempdir().unwrap();
    let pdf = temp.path().join("mixed.pdf");
    write_mixed_text_blank_pdf(&pdf);
    let output = run_worker(&pdf);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let fields = &response["records"][0]["fields"];
    assert_eq!(fields["pdf_pages_total"], 2);
    assert_eq!(fields["pdf_pages_with_text"], 1);
    assert_eq!(fields["pdf_pages_omitted"], 1);
    assert!(fields["pdf_ocr_warning"]
        .as_str()
        .unwrap()
        .contains("may need OCR"));
}

#[cfg(unix)]
#[test]
fn parent_drains_overflow_and_timeout_kills_worker_process_group() {
    let _env = env_guard();
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.pdf");
    fs::write(&input, b"unused by fake worker").unwrap();

    let overflow = temp.path().join("overflow-worker");
    fs::write(
        &overflow,
        b"#!/bin/sh\nhead -c 34603008 /dev/zero\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&overflow, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &overflow);
    let started = Instant::now();
    let mut sink = |_record| true;
    let error = xerj_autoindex::extract::pdf::extract(&input, &mut sink).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(error.to_string().contains("exceeded"), "{error:#}");

    let stderr_overflow = temp.path().join("stderr-overflow-worker");
    fs::write(
        &stderr_overflow,
        b"#!/bin/sh\nhead -c 131072 /dev/zero >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&stderr_overflow, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &stderr_overflow);
    let started = Instant::now();
    let error = xerj_autoindex::extract::pdf::extract(&input, &mut sink).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        error.to_string().contains("error output exceeded"),
        "{error:#}"
    );

    let timeout = temp.path().join("timeout-worker");
    fs::write(
        &timeout,
        b"#!/bin/sh\nsleep 60 &\necho $! > \"$2.pid\"\nwait\n",
    )
    .unwrap();
    fs::set_permissions(&timeout, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &timeout);
    xerj_autoindex::extract::pdf::configure_timeout(1);
    let error = xerj_autoindex::extract::pdf::extract(&input, &mut sink).unwrap_err();
    xerj_autoindex::extract::pdf::configure_timeout(120);
    std::env::remove_var("XERJ_PDF_WORKER_BIN");
    assert!(error.to_string().contains("timed out"), "{error:#}");

    let descendant = fs::read_to_string(format!("{}.pid", input.display())).unwrap();
    let proc_entry = std::path::PathBuf::from("/proc").join(descendant.trim());
    for _ in 0..50 {
        if !proc_entry.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("worker descendant {} survived timeout", descendant.trim());
}

#[cfg(unix)]
#[test]
fn pdf_worker_accepts_non_utf8_unix_paths() {
    let _env = env_guard();
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir().unwrap();
    let name = std::ffi::OsString::from_vec(b"quarterly-\xff.pdf".to_vec());
    let pdf = temp.path().join(name);
    write_prose_pdf(&pdf);

    let output = run_worker(&pdf);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["records"][0]["fields"]["page"], 1);
}

/// The parent spawn path (`extract` → `pre_exec`) is what #2336 broke: a
/// refused `RLIMIT_AS` failed `Command::spawn` before `exec`, so
/// `XERJ_PDF_WORKER_BIN` never ran. This calls that path with the real
/// worker. On Linux the 1536 MiB cap is applied; on a kernel that returns
/// `EINVAL` or `EPERM` the worker still starts.
#[cfg(unix)]
#[test]
fn parent_spawn_extracts_a_small_pdf_when_the_address_space_cap_is_refused_or_applied() {
    let _env = env_guard();
    let previous = std::env::var_os("XERJ_PDF_WORKER_BIN");
    std::env::set_var("XERJ_PDF_WORKER_BIN", env!("CARGO_BIN_EXE_xerj"));
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("XERJ_PDF_WORKER_BIN", value),
                None => std::env::remove_var("XERJ_PDF_WORKER_BIN"),
            }
        }
    }
    let _restore = Restore(previous);

    let temp = tempfile::tempdir().unwrap();
    let pdf = temp.path().join("quarterly-report.pdf");
    write_prose_pdf(&pdf);

    let mut bodies = Vec::new();
    let mut sink = |record: xerj_autoindex::extract::RawRecord| {
        if let Some(body) = record.fields.get("body").and_then(|value| value.as_str()) {
            bodies.push(body.to_string());
        }
        true
    };
    let stats = xerj_autoindex::extract::pdf::extract(&pdf, &mut sink)
        .expect("isolated PDF parser starts even if RLIMIT_AS is refused");
    assert!(stats.records >= 1, "records={}", stats.records);
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("operating margin improved")),
        "{bodies:?}"
    );
}
