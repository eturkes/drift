use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use drift_observer::{
    DriftError, Report, Result,
    adapters::codex_exec::{ImportOptions, import as import_codex_exec},
    aggregate::{AnalysisRun, aggregate_report},
    json::ensure_unique_keys,
    observer::{ObserverConfig, RUBRIC_VERSION, observe},
    parse::{MAX_FILE_BYTES, parse_trace},
    render::{inline, render_report},
    report_validation::validate_report,
};

const TRACE_SCHEMA: &str = include_str!("../schemas/trace.schema.json");
const JUDGMENT_SCHEMA: &str = include_str!("../schemas/codex-judgment.schema.json");
const REPORT_SCHEMA: &str = include_str!("../schemas/report.schema.json");
const USAGE: &str = "\
Drift - evidence-grounded agent-session observability

Usage:
  drift validate TRACE
  drift import codex-exec [OPTIONS] INPUT
  drift analyze [OPTIONS] TRACE
  drift render REPORT
  drift schema trace|judgment|report

Import options:
      --task TEXT          Original task (required)
      --constraint TEXT    Task constraint; repeatable
      --success TEXT       Success criterion; repeatable
  -o, --output PATH        Write trace to PATH instead of stdout

Analyze options:
  -o, --output PATH        Also write the self-contained JSON report
      --json               Print JSON instead of the human report
      --model SLUG         Pin the Codex model (default: Codex CLI default)
      --codex PATH         Codex executable (default: codex)
      --timeout-seconds N  Per-attempt timeout (default: 600)
      --attempts N         Judgment attempts, including one repair (default: 2)
  -h, --help               Show this help

TRACE is strict drift.trace/v1 JSONL. INPUT is Codex `exec --json` JSONL.
Use '-' to read TRACE, INPUT, or REPORT from stdin.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", inline(&error.to_string(), 4_000));
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        print!("{USAGE}");
        return Ok(());
    };
    match command.to_str() {
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("drift {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("validate") => validate_command(args.collect()),
        Some("import") => import_command(args.collect()),
        Some("analyze") => analyze_command(args.collect()),
        Some("render") => render_command(args.collect()),
        Some("schema") => schema_command(args.collect()),
        Some(other) => Err(DriftError::new(
            "E_ARGUMENT",
            format!("unknown command {other:?}; run 'drift --help'"),
        )),
        None => Err(DriftError::new("E_ARGUMENT", "command is not valid UTF-8")),
    }
}

struct ImportArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    options: ImportOptions,
}

fn import_command(args: Vec<OsString>) -> Result<()> {
    let options = parse_import_args(args)?;
    if let Some(output) = &options.output
        && path_aliases(&options.input, output)
    {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "import output must not overwrite the Codex input",
        ));
    }
    let source = read_bounded_input(&options.input, MAX_FILE_BYTES)?;
    let trace = import_codex_exec(&source, options.options)?;
    if let Some(path) = options.output {
        write_atomic(&path, &trace)
    } else {
        io::stdout()
            .lock()
            .write_all(&trace)
            .map_err(|error| DriftError::io("write imported trace to stdout", error))
    }
}

fn parse_import_args(args: Vec<OsString>) -> Result<ImportArgs> {
    let Some(format) = args.first().and_then(|value| value.to_str()) else {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "usage: drift import codex-exec [OPTIONS] INPUT",
        ));
    };
    if matches!(format, "-h" | "--help") {
        print!("{USAGE}");
        std::process::exit(0);
    }
    if format != "codex-exec" {
        return Err(DriftError::new(
            "E_ARGUMENT",
            format!("unknown import format {format:?}; expected 'codex-exec'"),
        ));
    }

    let mut task = None;
    let mut constraints = Vec::new();
    let mut success_criteria = Vec::new();
    let mut output = None;
    let mut input = None;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            Some("--task") => {
                index += 1;
                if task.is_some() {
                    return Err(DriftError::new(
                        "E_ARGUMENT",
                        "--task may be provided only once",
                    ));
                }
                task = Some(required_utf8(&args, index, "--task")?);
            }
            Some("--constraint") => {
                index += 1;
                constraints.push(required_utf8(&args, index, "--constraint")?);
            }
            Some("--success") => {
                index += 1;
                success_criteria.push(required_utf8(&args, index, "--success")?);
            }
            Some("-o" | "--output") => {
                index += 1;
                if output.is_some() {
                    return Err(DriftError::new(
                        "E_ARGUMENT",
                        "--output may be provided only once",
                    ));
                }
                output = Some(PathBuf::from(required_value(&args, index, "--output")?));
            }
            Some("--") => {
                index += 1;
                if index >= args.len() || input.is_some() || index + 1 != args.len() {
                    return Err(DriftError::new("E_ARGUMENT", "expected exactly one INPUT"));
                }
                input = Some(PathBuf::from(&args[index]));
            }
            Some(option) if option.starts_with('-') && option != "-" => {
                return Err(DriftError::new(
                    "E_ARGUMENT",
                    format!("unknown import option {option:?}"),
                ));
            }
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(DriftError::new("E_ARGUMENT", "expected exactly one INPUT")),
        }
        index += 1;
    }
    let input = input.ok_or_else(|| DriftError::new("E_ARGUMENT", "missing INPUT"))?;
    let task = task.ok_or_else(|| DriftError::new("E_ARGUMENT", "missing required --task"))?;
    if output.as_deref() == Some(Path::new("-")) {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "omit --output to write the imported trace to stdout",
        ));
    }
    Ok(ImportArgs {
        input,
        output,
        options: ImportOptions {
            task,
            constraints,
            success_criteria,
        },
    })
}

fn validate_command(args: Vec<OsString>) -> Result<()> {
    if args.len() != 1 {
        return Err(DriftError::new("E_ARGUMENT", "usage: drift validate TRACE"));
    }
    let trace = load_trace(Path::new(&args[0]))?;
    println!(
        "valid {}: {} events, completeness={}, {}",
        inline(trace.id(), 256),
        trace.events.len(),
        schema_label(trace.completeness()),
        trace.input_digest
    );
    for warning in &trace.warnings {
        println!("warning: {}", inline(warning, 1_000));
    }
    Ok(())
}

fn schema_label<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

struct AnalyzeArgs {
    trace: PathBuf,
    output: Option<PathBuf>,
    json: bool,
    observer: ObserverConfig,
}

fn analyze_command(args: Vec<OsString>) -> Result<()> {
    let options = parse_analyze_args(args)?;
    if let Some(output) = &options.output
        && path_aliases(&options.trace, output)
    {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "report output must not overwrite the input trace",
        ));
    }

    let trace = load_trace(&options.trace)?;
    let observation = observe(&trace, &options.observer)?;
    let report = aggregate_report(
        &trace,
        observation.judgment,
        AnalysisRun {
            rubric: RUBRIC_VERSION.into(),
            rubric_digest: observation.rubric_digest,
            rubric_source: observation.rubric_source,
            judgment_schema_digest: observation.judgment_schema_digest,
            judgment_schema_source: observation.judgment_schema_source,
            judge: "codex exec".into(),
            codex_cli: observation.codex_cli,
            model: observation.model,
            thread_id: observation.thread_id,
            attempts: observation.attempts,
        },
    )?;
    let json = report_json(&report)?;
    if let Some(path) = options.output {
        write_atomic(&path, &json)?;
    }
    if options.json {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&json)
            .and_then(|_| stdout.write_all(b"\n"))
            .map_err(|error| DriftError::io("write report to stdout", error))?;
    } else {
        print!("{}", render_report(&report));
    }
    Ok(())
}

fn parse_analyze_args(args: Vec<OsString>) -> Result<AnalyzeArgs> {
    let mut output = None;
    let mut json = false;
    let mut observer = ObserverConfig::default();
    let mut trace = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            Some("--json") => json = true,
            Some("-o" | "--output") => {
                index += 1;
                output = Some(PathBuf::from(required_value(&args, index, "--output")?));
            }
            Some("--model") => {
                index += 1;
                observer.model = Some(required_utf8(&args, index, "--model")?);
            }
            Some("--codex") => {
                index += 1;
                observer.codex = required_value(&args, index, "--codex")?.to_owned();
            }
            Some("--timeout-seconds") => {
                index += 1;
                let value = required_utf8(&args, index, "--timeout-seconds")?;
                let seconds = value.parse::<u64>().map_err(|_| {
                    DriftError::new("E_ARGUMENT", "--timeout-seconds must be an integer")
                })?;
                if !(1..=7200).contains(&seconds) {
                    return Err(DriftError::new(
                        "E_ARGUMENT",
                        "--timeout-seconds must be between 1 and 7200",
                    ));
                }
                observer.timeout = Duration::from_secs(seconds);
            }
            Some("--attempts") => {
                index += 1;
                let value = required_utf8(&args, index, "--attempts")?;
                observer.max_attempts = value
                    .parse::<usize>()
                    .map_err(|_| DriftError::new("E_ARGUMENT", "--attempts must be an integer"))?;
            }
            Some("--") => {
                index += 1;
                if index >= args.len() || trace.is_some() || index + 1 != args.len() {
                    return Err(DriftError::new("E_ARGUMENT", "expected exactly one TRACE"));
                }
                trace = Some(PathBuf::from(&args[index]));
            }
            Some(option) if option.starts_with('-') && option != "-" => {
                return Err(DriftError::new(
                    "E_ARGUMENT",
                    format!("unknown analyze option {option:?}"),
                ));
            }
            _ if trace.is_none() => trace = Some(PathBuf::from(arg)),
            _ => {
                return Err(DriftError::new("E_ARGUMENT", "expected exactly one TRACE"));
            }
        }
        index += 1;
    }
    let trace = trace.ok_or_else(|| DriftError::new("E_ARGUMENT", "missing TRACE"))?;
    if output.as_deref() == Some(Path::new("-")) {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "use --json for stdout; --output requires a file path",
        ));
    }
    Ok(AnalyzeArgs {
        trace,
        output,
        json,
        observer,
    })
}

fn render_command(args: Vec<OsString>) -> Result<()> {
    if args.len() != 1 {
        return Err(DriftError::new("E_ARGUMENT", "usage: drift render REPORT"));
    }
    let bytes = read_bounded_input(Path::new(&args[0]), MAX_FILE_BYTES * 2)?;
    ensure_unique_keys(&bytes)
        .map_err(|error| DriftError::new("E_REPORT_JSON", format!("invalid report: {error}")))?;
    let report: Report = serde_json::from_slice(&bytes)
        .map_err(|error| DriftError::new("E_REPORT_JSON", format!("invalid report: {error}")))?;
    validate_report(&report)?;
    print!("{}", render_report(&report));
    Ok(())
}

fn schema_command(args: Vec<OsString>) -> Result<()> {
    if args.len() != 1 {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "usage: drift schema trace|judgment|report",
        ));
    }
    match args[0].to_str() {
        Some("trace") => print!("{TRACE_SCHEMA}"),
        Some("judgment") => print!("{JUDGMENT_SCHEMA}"),
        Some("report") => print!("{REPORT_SCHEMA}"),
        _ => {
            return Err(DriftError::new(
                "E_ARGUMENT",
                "schema must be 'trace', 'judgment', or 'report'",
            ));
        }
    }
    Ok(())
}

fn load_trace(path: &Path) -> Result<drift_observer::Trace> {
    if path == Path::new("-") {
        parse_trace(BufReader::new(io::stdin().lock()))
    } else {
        let file = File::open(path).map_err(|error| DriftError::io("open trace", error))?;
        parse_trace(BufReader::new(file))
    }
}

fn read_bounded_input(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        io::stdin()
            .lock()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| DriftError::io("read stdin", error))?;
    } else {
        File::open(path)
            .map_err(|error| DriftError::io("open file", error))?
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| DriftError::io("read file", error))?;
    }
    if bytes.len() > limit {
        return Err(DriftError::new(
            "E_FILE_TOO_LARGE",
            format!("file exceeds {limit} bytes"),
        ));
    }
    Ok(bytes)
}

fn report_json(report: &Report) -> Result<Vec<u8>> {
    serde_json::to_vec(report)
        .map_err(|error| DriftError::new("E_INTERNAL", format!("serialize report: {error}")))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("report.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{name}.drift-tmp-{}-{nonce}", std::process::id()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| DriftError::io("create temporary output", error))?;
        file.write_all(bytes)
            .and_then(|_| {
                if bytes.ends_with(b"\n") {
                    Ok(())
                } else {
                    file.write_all(b"\n")
                }
            })
            .and_then(|_| file.sync_all())
            .map_err(|error| DriftError::io("write temporary output", error))?;
        fs::rename(&temporary, path)
            .map_err(|error| DriftError::io("replace output atomically", error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn path_aliases(input: &Path, output: &Path) -> bool {
    if input == Path::new("-") {
        return false;
    }
    if input == output {
        return true;
    }
    match (fs::canonicalize(input), fs::canonicalize(output)) {
        (Ok(input), Ok(output)) => input == output,
        _ => false,
    }
}

fn required_value<'a>(args: &'a [OsString], index: usize, option: &str) -> Result<&'a OsString> {
    args.get(index)
        .ok_or_else(|| DriftError::new("E_ARGUMENT", format!("{option} requires a value")))
}

fn required_utf8(args: &[OsString], index: usize, option: &str) -> Result<String> {
    required_value(args, index, option)?
        .to_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            DriftError::new(
                "E_ARGUMENT",
                format!("{option} requires a nonempty UTF-8 value"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        JUDGMENT_SCHEMA, REPORT_SCHEMA, TRACE_SCHEMA, parse_analyze_args, parse_import_args,
    };

    #[test]
    fn parses_codex_import_task_contract() {
        let args = [
            "codex-exec",
            "--task",
            "Check CI",
            "--constraint",
            "Read only",
            "--success",
            "Report the job result",
            "--output",
            "trace.jsonl",
            "codex.jsonl",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let parsed = parse_import_args(args).unwrap();
        assert_eq!(parsed.input.to_str(), Some("codex.jsonl"));
        assert_eq!(
            parsed.output.as_deref().and_then(|path| path.to_str()),
            Some("trace.jsonl")
        );
        assert_eq!(parsed.options.task, "Check CI");
        assert_eq!(parsed.options.constraints, ["Read only"]);
        assert_eq!(parsed.options.success_criteria, ["Report the job result"]);
    }

    #[test]
    fn parses_analyze_options_without_shell_interpretation() {
        let args = [
            "--model",
            "gpt-test",
            "--codex",
            "/path with spaces/codex",
            "--timeout-seconds",
            "12",
            "--attempts",
            "1",
            "--json",
            "trace.jsonl",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let parsed = parse_analyze_args(args).unwrap();
        assert_eq!(parsed.trace.to_str(), Some("trace.jsonl"));
        assert_eq!(parsed.observer.model.as_deref(), Some("gpt-test"));
        assert_eq!(parsed.observer.codex, "/path with spaces/codex");
        assert_eq!(parsed.observer.timeout.as_secs(), 12);
        assert_eq!(parsed.observer.max_attempts, 1);
        assert!(parsed.json);
    }

    #[test]
    fn distributed_schemas_are_json_objects() {
        for schema in [TRACE_SCHEMA, JUDGMENT_SCHEMA, REPORT_SCHEMA] {
            let value: serde_json::Value = serde_json::from_str(schema).unwrap();
            assert!(value.is_object());
        }
    }
}
