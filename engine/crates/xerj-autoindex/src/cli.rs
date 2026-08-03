//! Hand-rolled arg parser (house style of xerj-server — no clap).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct IndexCfg {
    pub root: PathBuf,
    pub url: String,
    pub api_key: Option<String>,
    pub workers: usize,
    pub pdf_workers: usize,
    pub pdf_timeout_secs: u64,
    pub bulk_mb: usize,
    pub bulk_timeout_secs: u64,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
    pub fresh: bool,
    pub follow_symlinks: bool,
    pub max_file_gb: u64,
    pub sample: usize,
    pub no_semantic: bool,
    /// Second-brain name; None derives it from the root folder basename.
    pub brain: Option<String>,
    /// Disable edge detection entirely (no `.xerj-memory-*-edges` writes).
    pub no_graph: bool,
    pub dry_run: bool,
    pub json: bool,
    pub quiet: bool,
}

#[derive(Debug, Clone)]
pub struct MapCfg {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub json: bool,
    pub dataset: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StatusCfg {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Cmd {
    Index(IndexCfg),
    Map(MapCfg),
    Status(StatusCfg),
    Help,
}

const FRESH_HELP: &str =
    "start without resume state only when the selected state directory has no \
durable plan; an existing plan is refused and destination records are never reset";

pub fn print_help() {
    println!(
        "xerj autoindex — point it at any folder and make the contents AI-searchable, zero config\n\
         \n\
         USAGE:\n\
             xerj autoindex <folder> [OPTIONS]     discover + index a folder\n\
             xerj autoindex map [OPTIONS]          print the discovered data map\n\
             xerj autoindex status [OPTIONS]       resume-journal + index progress view\n\
         \n\
         OPTIONS:\n\
             --url <U>            ES-compat endpoint (default http://localhost:9200)\n\
             --api-key <K>        Authorization header (or env XERJ_API_KEY)\n\
             --workers <N>        extract workers (default min(cores,8))\n\
             --pdf-workers <N>    concurrent PDF parser processes (default min(cores,4); max 4)\n\
             --pdf-timeout-secs <N> per-PDF parser timeout (default 120; max 3600)\n\
             --bulk-mb <N>        bulk cut size in MB (default 8)\n\
             --bulk-timeout-secs <N>\n\
                                  bulk HTTP request timeout in seconds (default 300;\n\
                                  valid range 1..=3600)\n\
             --prefix <P>         index prefix (default ax)\n\
             --state-dir <PATH>   resume journal location (default ~/.xerj/autoindex/<hash>/)\n\
             --fresh              {fresh_help}\n\
             --follow-symlinks    follow symlinks (loop-safe); off by default\n\
             --max-file-gb <N>    skip+record oversized non-streamable files (default 2)\n\
             --sample <N>         records sampled per file for inference (default 500)\n\
             --no-semantic        skip semantic_text on body fields (pure BM25+keyword)\n\
             --brain <NAME>       second-brain name; relationship edges land in\n\
                                  .xerj-memory-<NAME>-edges (default: folder name slug)\n\
             --no-graph           skip relationship detection (wikilinks, local links,\n\
                                  section order, directory chains) — no edges are written\n\
             --dry-run            walk+sniff+infer, print the plan, index nothing\n\
             --json               machine-readable output (map: raw catalog docs)\n\
             --quiet              errors only\n\
             --dataset <SLUG>     (map) show a single dataset\n\
             --help, -h           this help\n\
         \n\
         PDF EXTRACTION:\n\
             Each PDF uses a fresh process. Limits: 512 MiB input, 32 MiB worker output,\n\
             100,000 pages, and 1.5 GiB address space on Unix. This is crash/resource\n\
             isolation, not a security sandbox; non-Unix platforms have no OS memory cap.\n\
             Image-only pages need OCR. Page parse failures reject the whole PDF instead\n\
             of silently creating a partial index. XERJ_PDF_WORKER_BIN is a trusted,\n\
             developer-only executable override.\n\
             Current cost: phase-A sampling parses/materializes each complete PDF, and\n\
             phase-B indexing parses it again. A framed, early-stop protocol is planned;\n\
             use fewer --pdf-workers when parent memory is constrained.\n\
         \n\
         EMBEDDINGS:\n\
             autoindex sends semantic_text to the running server; it does not choose the\n\
             server's embedding backend. The default is lexical (not neural). For the\n\
             experimental ONNX backend, start xerj with `--embed-mode onnx-experimental\n\
             --onnx-model MODEL.onnx --onnx-tokenizer tokenizer.json`, then run autoindex.\n\
             ONNX runs only for fields inferred as semantic_text (normally long body text;\n\
             short/structured datasets may infer none). Use --dry-run or `autoindex map` to\n\
             confirm a semantic field before attributing an indexing result to embeddings.\n\
         \n\
         RESUME POLICY:\n\
             A durable plan supports no-op resume and same-path content replacement.\n\
             Added or removed content groups are refused before remote mutation.\n\
             An independent rebuild needs a new --state-dir, new --prefix, and, when\n\
             graph detection is enabled, new --brain (or --no-graph). Validate before\n\
             switching readers; the shared autoindex-catalog and old target require\n\
             explicit, validated cleanup.\n\
         \n\
         EXIT CODES: 0 complete; 3 completed-with-junk (junk recorded, never fatal);\n\
                     2 usage; 1 endpoint/journal failure or unsupported corpus delta\n",
        fresh_help = FRESH_HELP
    );
}

pub fn parse(args: Vec<String>) -> Result<Cmd, String> {
    let mut it = args.into_iter().peekable();
    let mut folder: Option<PathBuf> = None;
    let mut sub: Option<String> = None;

    let mut url = "http://localhost:9200".to_string();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .min(8);
    let mut pdf_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(4);
    let mut pdf_timeout_secs = 120u64;
    let mut bulk_mb = 8usize;
    let mut bulk_timeout_secs = 300u64;
    let mut bulk_timeout_explicit = false;
    let mut prefix = "ax".to_string();
    let mut state_dir: Option<PathBuf> = None;
    let mut fresh = false;
    let mut follow_symlinks = false;
    let mut max_file_gb = 2u64;
    let mut sample = 500usize;
    let mut no_semantic = false;
    let mut brain: Option<String> = None;
    let mut no_graph = false;
    let mut dry_run = false;
    let mut json = false;
    let mut quiet = false;
    let mut dataset: Option<String> = None;

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--url" => url = it.next().ok_or("--url needs a value")?,
            "--api-key" => api_key = it.next(),
            "--workers" => {
                workers = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--workers needs a number")?
            }
            "--pdf-workers" => {
                pdf_workers = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=4).contains(n))
                    .ok_or("--pdf-workers needs a number from 1 to 4")?
            }
            "--pdf-timeout-secs" => {
                pdf_timeout_secs = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=3600).contains(n))
                    .ok_or("--pdf-timeout-secs needs a number from 1 to 3600")?
            }
            "--bulk-mb" => {
                bulk_mb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--bulk-mb needs a number")?
            }
            "--bulk-timeout-secs" => {
                bulk_timeout_explicit = true;
                bulk_timeout_secs = it
                    .next()
                    .ok_or("--bulk-timeout-secs needs a value in seconds")?
                    .parse()
                    .map_err(|_| "--bulk-timeout-secs needs an integer in the range 1..=3600")?;
                if !(1..=3_600).contains(&bulk_timeout_secs) {
                    return Err("--bulk-timeout-secs must be in the range 1..=3600 seconds".into());
                }
            }
            "--in-flight" => {
                let _ = it.next(); // reserved (bulks are worker-synchronous in v1)
            }
            "--prefix" => prefix = it.next().ok_or("--prefix needs a value")?,
            "--state-dir" => state_dir = it.next().map(PathBuf::from),
            "--fresh" => fresh = true,
            "--follow-symlinks" => follow_symlinks = true,
            "--max-file-gb" => {
                max_file_gb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--max-file-gb needs a number")?
            }
            "--sample" => {
                sample = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--sample needs a number")?
            }
            "--no-semantic" => no_semantic = true,
            "--brain" => {
                let name = it.next().ok_or("--brain needs a value")?;
                // Fail at parse time — a bad brain name after a long Phase A
                // would be a rude place to learn about the '-edges' rule.
                crate::detect::validate_brain(&name)
                    .map_err(|reason| format!("--brain {name}: {reason}"))?;
                brain = Some(name);
            }
            "--no-graph" => no_graph = true,
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "--md" => json = false,
            "--quiet" => quiet = true,
            "--dataset" => dataset = it.next(),
            "--help" | "-h" => return Ok(Cmd::Help),
            "map" if sub.is_none() && folder.is_none() => sub = Some("map".into()),
            "status" if sub.is_none() && folder.is_none() => sub = Some("status".into()),
            other if !other.starts_with('-') && folder.is_none() && sub.is_none() => {
                folder = Some(PathBuf::from(other))
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let prefix = crate::dataset::sanitize_slug(&prefix);
    if prefix.is_empty() {
        return Err("--prefix must contain at least one [a-z0-9] character".into());
    }

    match (sub.as_deref(), folder) {
        (Some("map"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex map`".into())
        }
        (Some("map"), _) => Ok(Cmd::Map(MapCfg {
            url,
            api_key,
            prefix,
            json,
            dataset,
        })),
        (Some("status"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex status`".into())
        }
        (Some("status"), _) => Ok(Cmd::Status(StatusCfg {
            url,
            api_key,
            prefix,
            state_dir,
        })),
        (None, Some(root)) => Ok(Cmd::Index(IndexCfg {
            root,
            url,
            api_key,
            workers: workers.max(1),
            pdf_workers,
            pdf_timeout_secs,
            bulk_mb: bulk_mb.clamp(1, 24),
            bulk_timeout_secs,
            prefix,
            state_dir,
            fresh,
            follow_symlinks,
            max_file_gb,
            sample: sample.max(50),
            no_semantic,
            brain,
            no_graph,
            dry_run,
            json,
            quiet,
        })),
        _ => Ok(Cmd::Help),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Cmd};

    fn index(args: &[&str]) -> super::IndexCfg {
        match parse(args.iter().map(|s| s.to_string()).collect()).unwrap() {
            Cmd::Index(cfg) => cfg,
            other => panic!("expected index config, got {other:?}"),
        }
    }

    #[test]
    fn bulk_timeout_defaults_to_300_seconds() {
        assert_eq!(index(&["data"]).bulk_timeout_secs, 300);
    }

    #[test]
    fn fresh_help_warns_that_it_is_not_destination_reconciliation() {
        assert!(super::FRESH_HELP.contains("no durable plan"));
        assert!(super::FRESH_HELP.contains("existing plan is refused"));
        assert!(super::FRESH_HELP.contains("destination records are never reset"));
    }

    #[test]
    fn graph_is_on_by_default_with_derived_brain() {
        let cfg = index(&["data"]);
        assert!(!cfg.no_graph);
        assert_eq!(cfg.brain, None, "brain derives from the folder at run time");
        assert!(index(&["data", "--no-graph"]).no_graph);
        assert_eq!(
            index(&["data", "--brain", "notes"]).brain.as_deref(),
            Some("notes")
        );
    }

    #[test]
    fn brain_names_are_validated_at_parse_time() {
        for bad in ["kb-edges", "Notes", "-x", "a..b"] {
            let err = parse(
                ["data", "--brain", bad]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )
            .unwrap_err();
            assert!(err.contains("--brain"), "{err}");
        }
    }

    #[test]
    fn bulk_timeout_accepts_custom_bounded_value() {
        assert_eq!(
            index(&["data", "--bulk-timeout-secs", "3600"]).bulk_timeout_secs,
            3600
        );
    }

    #[test]
    fn bulk_timeout_rejects_missing_non_numeric_zero_and_too_large() {
        for args in [
            vec!["data", "--bulk-timeout-secs"],
            vec!["data", "--bulk-timeout-secs", "slow"],
            vec!["data", "--bulk-timeout-secs", "0"],
            vec!["data", "--bulk-timeout-secs", "3601"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("--bulk-timeout-secs"), "{err}");
        }
    }

    #[test]
    fn bulk_timeout_is_rejected_for_non_index_subcommands_in_any_position() {
        for args in [
            vec!["map", "--bulk-timeout-secs", "900"],
            vec!["--bulk-timeout-secs", "900", "map"],
            vec!["status", "--bulk-timeout-secs", "900"],
            vec!["--bulk-timeout-secs", "900", "status"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("applies only to indexing"), "{err}");
        }
    }
}
