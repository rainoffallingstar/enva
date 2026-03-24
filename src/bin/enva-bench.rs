use clap::{Parser, ValueEnum};
use enva::env_run::{execute_env_run, EnvRunArgs};
use enva::{PackageManager, PackageManagerDetector, Result};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::process::Command as AsyncCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchmarkMode {
    Enva,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Csv,
}

#[derive(Debug, Parser)]
#[command(name = "enva-bench")]
#[command(about = "Benchmark enva run-path cold/hot behavior")]
struct Cli {
    #[arg(long, value_name = "ENV")]
    env_name: String,

    #[arg(long, value_name = "CMD", default_value = "true")]
    command: String,

    #[arg(long, value_enum)]
    pm: Option<PackageManager>,

    #[arg(long, default_value_t = 5)]
    iterations: usize,

    #[arg(long, default_value_t = false)]
    verbose: bool,

    #[arg(long, value_name = "DIR", default_value = ".")]
    cwd: PathBuf,

    #[arg(long, default_value_t = false)]
    compare_native: bool,

    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug)]
struct BenchmarkSummary {
    mode: BenchmarkMode,
    package_manager: PackageManager,
    env_name: String,
    command: String,
    iterations: usize,
    cold: Duration,
    hot_avg: Duration,
    hot_min: Duration,
    hot_max: Duration,
}

fn format_duration(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1000.0)
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn average_duration(samples: &[Duration]) -> Duration {
    let total_secs: f64 = samples.iter().map(Duration::as_secs_f64).sum();
    Duration::from_secs_f64(total_secs / samples.len() as f64)
}

fn benchmark_args(cli: &Cli) -> EnvRunArgs {
    EnvRunArgs {
        name: Some(cli.env_name.clone()),
        pm: cli.pm,
        prefix: None,
        command: Some(cli.command.clone()),
        script: None,
        args: vec![],
        cwd: cli.cwd.clone(),
        env: vec![],
        no_capture: true,
    }
}

fn detect_package_manager(cli: &Cli) -> Result<PackageManager> {
    if let Some(pm) = cli.pm {
        return Ok(pm);
    }

    let mut detector = PackageManagerDetector::new();
    detector.detect_with_env_override()
}

async fn run_enva_once(cli: &Cli, args: &EnvRunArgs) -> Result<()> {
    execute_env_run(args.clone(), cli.verbose).await
}

async fn run_native_once(cli: &Cli, pm: PackageManager) -> Result<()> {
    let mut cmd = AsyncCommand::new(pm.command());
    cmd.arg("run")
        .arg("-n")
        .arg(&cli.env_name)
        .arg("bash")
        .arg("-lc")
        .arg(&cli.command)
        .current_dir(&cli.cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let status = cmd.status().await.map_err(|error| {
        enva::EnvError::Execution(format!("Failed to execute native benchmark: {}", error))
    })?;

    if !status.success() {
        return Err(enva::EnvError::Execution(format!(
            "Native benchmark command failed with exit code {:?}",
            status.code()
        )));
    }

    Ok(())
}

async fn run_benchmark(cli: &Cli, mode: BenchmarkMode) -> Result<BenchmarkSummary> {
    let package_manager = detect_package_manager(cli)?;
    let args = benchmark_args(cli);

    let cold_start = Instant::now();
    match mode {
        BenchmarkMode::Enva => run_enva_once(cli, &args).await?,
        BenchmarkMode::Native => run_native_once(cli, package_manager).await?,
    }
    let cold = cold_start.elapsed();

    let mut hot_samples = Vec::with_capacity(cli.iterations);
    for _ in 0..cli.iterations {
        let start = Instant::now();
        match mode {
            BenchmarkMode::Enva => run_enva_once(cli, &args).await?,
            BenchmarkMode::Native => run_native_once(cli, package_manager).await?,
        }
        hot_samples.push(start.elapsed());
    }

    Ok(BenchmarkSummary {
        mode,
        package_manager,
        env_name: cli.env_name.clone(),
        command: cli.command.clone(),
        iterations: cli.iterations,
        cold,
        hot_avg: average_duration(&hot_samples),
        hot_min: hot_samples.iter().copied().min().unwrap_or_default(),
        hot_max: hot_samples.iter().copied().max().unwrap_or_default(),
    })
}

fn mode_label(mode: BenchmarkMode) -> &'static str {
    match mode {
        BenchmarkMode::Enva => "enva",
        BenchmarkMode::Native => "native",
    }
}

fn print_text_summary(summary: &BenchmarkSummary) {
    println!("{} {}", mode_label(summary.mode), summary.package_manager);
    println!("  env: {}", summary.env_name);
    println!("  command: {}", summary.command);
    println!("  cold run: {}", format_duration(summary.cold));
    println!("  hot runs: {} iterations", summary.iterations);
    println!("    avg: {}", format_duration(summary.hot_avg));
    println!("    min: {}", format_duration(summary.hot_min));
    println!("    max: {}", format_duration(summary.hot_max));
}

fn print_csv_header() {
    println!(
        "mode,package_manager,env_name,command,iterations,cold_ms,hot_avg_ms,hot_min_ms,hot_max_ms"
    );
}

fn escape_csv(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn print_csv_row(summary: &BenchmarkSummary) {
    println!(
        "{},{},{},{},{},{:.2},{:.2},{:.2},{:.2}",
        mode_label(summary.mode),
        summary.package_manager,
        escape_csv(&summary.env_name),
        escape_csv(&summary.command),
        summary.iterations,
        duration_ms(summary.cold),
        duration_ms(summary.hot_avg),
        duration_ms(summary.hot_min),
        duration_ms(summary.hot_max),
    );
}

fn print_json(summaries: &[BenchmarkSummary]) {
    let rows: Vec<_> = summaries
        .iter()
        .map(|summary| {
            json!({
                "mode": mode_label(summary.mode),
                "package_manager": summary.package_manager.to_string(),
                "env_name": summary.env_name,
                "command": summary.command,
                "iterations": summary.iterations,
                "cold_ms": duration_ms(summary.cold),
                "hot_avg_ms": duration_ms(summary.hot_avg),
                "hot_min_ms": duration_ms(summary.hot_min),
                "hot_max_ms": duration_ms(summary.hot_max),
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}

fn emit_results(format: OutputFormat, summaries: &[BenchmarkSummary]) {
    match format {
        OutputFormat::Text => {
            for (index, summary) in summaries.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                print_text_summary(summary);
            }
        }
        OutputFormat::Json => print_json(summaries),
        OutputFormat::Csv => {
            print_csv_header();
            for summary in summaries {
                print_csv_row(summary);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut summaries = vec![run_benchmark(&cli, BenchmarkMode::Enva).await?];

    if cli.compare_native {
        summaries.push(run_benchmark(&cli, BenchmarkMode::Native).await?);
    }

    emit_results(cli.format, &summaries);
    Ok(())
}
