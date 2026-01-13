# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**enva** is a lightweight, standalone micromamba environment manager designed for bioinformatics workflows. It's a Rust CLI tool that manages 4 pre-configured conda/micromamba environments for genomics and bioinformatics analysis. The project was extracted from xdxtools-rs with focus on minimalism and performance.

**Key Metrics:**
- Binary size: 5.4MB (89% smaller than xdxtools ~50MB)
- Startup time: ~0.2s (10-15x faster than xdxtools)
- Memory usage: ~30MB (70% less than xdxtools)

## Build and Development Commands

### Basic Build Commands
```bash
# Development build
cargo build

# Release build (optimized, recommended)
cargo build --release

# Check for warnings
cargo clippy

# Fix clippy warnings automatically
cargo clippy --fix

# Run all tests
cargo test

# Run tests with verbose output
cargo test -- --nocapture

# Format code
cargo fmt

# Format and check formatting
cargo fmt --check
```

### Running the Application
```bash
# Build and run (dev mode)
cargo run -- --help

# Build and run (release mode, faster)
cargo run --release -- --help

# Create all environments
cargo run --release -- create --all

# Run specific command
cargo run --release -- list

# Enable verbose logging
cargo run --release -- -v create --core

# Dry-run mode (validate without creating)
cargo run --release -- --dry-run create --all

# JSON output
cargo run --release -- --json list
```

### Release Building
```bash
# Create release builds for all platforms
./build-release.sh

# Manual cross-compilation (requires target installation)
rustup target add x86_64-unknown-linux-gnu
rustup target add x86_64-apple-darwin
rustup target add x86_64-pc-windows-msvc
rustup target add aarch64-apple-darwin

cargo build --release --target x86_64-unknown-linux-gnu
cargo build --release --target x86_64-apple-darwin
cargo build --release --target x86_64-pc-windows-msvc
cargo build --release --target aarch64-apple-darwin
```

## High-Level Architecture

### Module Structure

```
src/
├── main.rs              # CLI entry point, argument parsing
├── lib.rs               # Library exports, constants, initialization
├── error.rs             # Error types (EnvError enum)
├── micromamba.rs        # Core micromamba integration (largest file)
├── env.rs               # Environment command handlers
└── env_run.rs           # Command execution in environments
```

### Core Modules

#### 1. **micromamba.rs** (1,601 lines)
The heart of the application - handles all micromamba operations:
- Automatic micromamba installation/download
- Environment creation from YAML configs
- Package installation
- Environment validation
- Tool-to-environment mapping
- Async command execution

**Key Components:**
- `MicromambaManager` - Global manager with lazy initialization
- `MicromambaEnvironment` struct - Environment configuration
- `TOOL_ENVIRONMENT_MAP` - Maps tools to their default environments

**4 Pre-configured Environments:**
1. `xdxtools-core` - Core bioinformatics tools (FastQC, MultiQC, Bismark, STAR, BWA, etc.)
2. `xdxtools-r` - R/Bioconductor packages with Qualimap
3. `xdxtools-snakemake` - Workflow engine and dependencies
4. `xdxtools-extra` - Advanced visualization and analysis tools

#### 2. **env.rs** (752 lines)
Environment command handlers and CLI argument structures:
- `EnvCommand` enum - Subcommand definitions
- `EnvCreateArgs`, `EnvListArgs`, etc. - Command argument structs
- Command execution orchestrator

#### 3. **env_run.rs** (242 lines)
Command execution within environments:
- Running scripts/commands in specific environments
- Environment variable handling
- Working directory management

#### 4. **error.rs** (247 lines)
Error handling:
- `EnvError` enum with 15+ error variants
- Custom error types for configuration, validation, network, etc.

#### 5. **main.rs** (56 lines) & **lib.rs** (39 lines)
- CLI entry point with clap parser
- Library exports and constants
- Startup banner display

### Configuration System

YAML configuration files in `src/configs/`:
- `xdxtools-core.yaml` - Core bioinformatics tools
- `xdxtools-r.yaml` - R/Bioconductor packages
- `xdxtools-snakemake.yaml` - Workflow engine
- `xdxtools-extra.yaml` - Additional tools

Each config defines:
- Environment name
- Channels (bioconda, conda-forge)
- Dependencies with specific versions

### Data Flow

```
CLI Args → main.rs → env.rs → micromamba.rs → MicromambaManager
                                                    ↓
                                            Execute micromamba commands
                                                    ↓
                                            Environment Operations
```

**Typical Workflows:**

1. **Create Environment:**
   - User runs `enva create --core`
   - `env.rs` handles command
   - `micromamba.rs` loads YAML config
   - MicromambaManager creates environment

2. **Run Command:**
   - User runs `enva run --name xdxtools-r --command "R --version"`
   - `env_run.rs` executes command in environment
   - Uses micromamba run command

3. **Install Packages:**
   - User runs `enva install --name xdxtools-core --packages "fastqc,multiqc"`
   - Installs packages into specified environment

## Key Dependencies

- **clap** - CLI argument parsing with derive macros
- **tokio** - Async runtime for concurrent operations
- **async-trait** - Async trait support
- **tracing** & **tracing-subscriber** - Structured logging
- **serde** - Serialization/deserialization
- **reqwest** - HTTP client for downloading micromamba
- **indicatif** - Progress bars
- **thiserror** - Error handling

## Important Constants

Defined in `src/lib.rs`:
```rust
pub const CORE_ENV_NAME: &str = "xdxtools-core";
pub const R_ENV_NAME: &str = "xdxtools-r";
pub const SNAKEMAKE_ENV_NAME: &str = "xdxtools-snakemake";
pub const EXTRA_ENV_NAME: &str = "xdxtools-extra";
```

## Development Patterns

### Async/Await Usage
- All micromamba operations are async
- Uses tokio::process::Command for async subprocess execution
- Global manager uses Arc<Mutex<Option<MicromambaManager>>>

### Error Handling
- All functions return `Result<T, EnvError>`
- EnvError variants for different error categories
- Uses thiserror for ergonomic error messages

### Configuration Loading
- YAML configs embedded at compile time in `src/configs/`
- Can also load custom YAML files via `--yaml` flag
- Uses serde_yaml for parsing

### Tool Mapping
`TOOL_ENVIRONMENT_MAP` in micromamba.rs defines which tools belong to which environment:
```rust
("fastqc", "xdxtools-core"),
("qualimap", "xdxtools-r"),
("snakemake", "xdxtools-snakemake"),
("bedtools", "xdxtools-extra"),
```

## Common Development Tasks

### Adding a New Environment
1. Create YAML file in `src/configs/`
2. Add environment name constant to `lib.rs`
3. Update `TOOL_ENVIRONMENT_MAP` if needed
4. Add command-line flag in `env.rs` (EnvCreateArgs)
5. Add handling in environment creation logic

### Adding a New Command
1. Define struct in `env.rs` (e.g., `EnvNewCommandArgs`)
2. Add variant to `EnvCommand` enum
3. Implement handler in `execute_env_command`
4. Add to CLI help text

### Modifying Environment Configs
1. Edit YAML files in `src/configs/`
2. Test with `--dry-run` flag
3. Validate with `enva validate --all`

## Testing

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture

# Run integration tests (if any)
cargo test --test integration
```

**Note:** No formal test suite found in current codebase. Consider adding tests for:
- YAML configuration validation
- Environment creation
- Command execution
- Error handling

## Release Process

1. Update version in `Cargo.toml`
2. Run `cargo build --release`
3. Execute `./build-release.sh` for all platforms
4. Test binaries before distribution
5. Release binaries stored in `release-YYYYMMDD-HHMMSS/` directory

## Cross-Platform Considerations

- Uses `which` crate for finding executables
- Path handling via `PathBuf`
- Unix permission handling in micromamba.rs (PermissionsExt)
- Windows, Linux, and macOS support

## Performance Optimizations

Already applied in Cargo.toml:
- LTO (Link Time Optimization) enabled
- Single codegen unit
- Symbols stripped
- Panic = abort for smaller binaries

## Key Files to Know

- **`src/micromamba.rs`** - Main logic, 1601 lines, start here for understanding core functionality
- **`src/env.rs`** - CLI command handlers
- **`src/configs/*.yaml`** - Environment definitions
- **`Cargo.toml`** - Dependencies and build configuration
- **`README.md`** - User documentation and usage examples

## Working with Existing Code

1. **Read micromamba.rs first** - Contains the core logic and data structures
2. **Check configuration files** - Understand what each environment contains
3. **Review error types** - In error.rs to understand failure modes
4. **Test with dry-run** - Use `--dry-run` flag to validate changes without side effects

## Migration from xdxtools

This project was extracted from xdxtools-rs with:
- Same micromamba logic (100% code reuse)
- Simplified CLI (removed xdxtools-specific options)
- Reduced dependencies
- Improved performance through optimization

## Project-Specific Conventions

- Uses async/await throughout
- Global manager pattern for micromamba
- Structured logging with tracing crate
- Progress bars for long operations
- JSON output support via `--json` flag
- Dry-run mode for safe validation

## Known Limitations

- Requires micromamba to be installed or will download automatically
- 4 pre-configured environments only (not arbitrary environments)
- Limited test coverage
- No plugin system for custom tools
