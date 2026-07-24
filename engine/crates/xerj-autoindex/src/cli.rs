//! Hand-rolled arg parser (house style of xerj-server — no clap).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct IndexCfg {
    pub root: PathBuf,
    pub url: String,
    pub api_key: Option<String>,
    pub workers: usize,
    pub bulk_mb: usize,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
    pub fresh: bool,
    pub follow_symlinks: bool,
    pub max_file_gb: u64,
    pub sample: usize,
    pub no_semantic: bool,
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
             --bulk-mb <N>        bulk cut size in MB (default 8)\n\
             --prefix <P>         index prefix (default ax)\n\
             --state-dir <PATH>   resume journal location (default ~/.xerj/autoindex/<hash>/)\n\
             --fresh              ignore existing journal, restart (ids stay idempotent)\n\
             --follow-symlinks    follow symlinks (loop-safe); off by default\n\
             --max-file-gb <N>    skip+record oversized non-streamable files (default 2)\n\
             --sample <N>         records sampled per file for inference (default 500)\n\
             --no-semantic        skip semantic_text on body fields (pure BM25+keyword)\n\
             --dry-run            walk+sniff+infer, print the plan, index nothing\n\
             --json               machine-readable output (map: raw catalog docs)\n\
             --quiet              errors only\n\
             --dataset <SLUG>     (map) show a single dataset\n\
             --help, -h           this help\n\
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
         EXIT CODES: 0 complete; 3 completed-with-junk (junk recorded, never fatal);\n\
                     2 usage; 1 endpoint unreachable / journal-config mismatch\n"
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
    let mut bulk_mb = 8usize;
    let mut prefix = "ax".to_string();
    let mut state_dir: Option<PathBuf> = None;
    let mut fresh = false;
    let mut follow_symlinks = false;
    let mut max_file_gb = 2u64;
    let mut sample = 500usize;
    let mut no_semantic = false;
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
            "--bulk-mb" => {
                bulk_mb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--bulk-mb needs a number")?
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
        (Some("map"), _) => Ok(Cmd::Map(MapCfg {
            url,
            api_key,
            prefix,
            json,
            dataset,
        })),
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
            bulk_mb: bulk_mb.clamp(1, 24),
            prefix,
            state_dir,
            fresh,
            follow_symlinks,
            max_file_gb,
            sample: sample.max(50),
            no_semantic,
            dry_run,
            json,
            quiet,
        })),
        _ => Ok(Cmd::Help),
    }
}
