# enva - Lightweight Micromamba Environment Manager

A lightweight, standalone micromamba environment manager for bioinformatics workflows, extracted from xdxtools-rs.

## 🚀 Features

- **Automatic Micromamba Installation**: Downloads and installs micromamba if not found
- **Multi-Platform Support**: Linux, macOS, Windows
- **4 Pre-configured Environments**:
  - `xdxtools-core`: Core bioinformatics tools (FastQC, MultiQC, Bismark, etc.)
  - `xdxtools-r`: R/Bioconductor packages with Qualimap
  - `xdxtools-snakemake`: Workflow engine and dependencies
  - `xdxtools-extra`: Advanced visualization and analysis tools

- **Complete Environment Management**:
  - Create environments from YAML configs
  - List and validate environments
  - Install packages into environments
  - Run commands/scripts in environments
  - Remove environments

- **Performance Optimizations**:
  - 2-3x faster than conda
  - 30% smaller disk footprint
  - Binary size: 5.4MB (release) vs 50MB (xdxtools)

## 📦 Installation

### Option 1: Download Pre-built Binary

Download the latest release from the `release-YYYYMMDD-HHMMSS/` directory:
- `enva-windows-x86_64.exe` (Windows)
- `enva-linux-x86_64` (Linux)
- `enva-macos-x86_64` (macOS Intel)
- `enva-macos-aarch64` (macOS Apple Silicon)

### Option 2: Build from Source

```bash
git clone <repository>
cd enva
cargo build --release
```

## 🎯 Usage

### Create Environments

```bash
# Create all environments
./enva create --all

# Create specific environment
./enva create --core
./enva create --r
./enva create --snakemake
./enva create --extra

# Create with custom name
./enva create --name my-env

# Dry-run validation
./enva --dry-run create --all
```

### List Environments

```bash
# List all environments
./enva list

# Detailed view
./enva list --detailed

# JSON output
./enva --json list
```

### Run Commands

```bash
# Run command in environment
./enva run --name xdxtools-r --command "R --version"

# Run script with arguments
./enva run --name xdxtools-core --script my_analysis.R -- arg1 arg2

# Set environment variables
./enva run --name xdxtools-r --command "echo $MY_VAR" --env MY_VAR=value

# Specify working directory
./enva run --name xdxtools-core --script process.sh --cwd /path/to/work
```

### Install Packages

```bash
# Install packages in environment
./enva install --name xdxtools-core --packages "fastqc,multiqc"

# Install in xdxtools-r environment
./enva install --name xdxtools-r --packages "dplyr,ggplot2"
```

### Validate Environments

```bash
# Validate all environments
./enva validate --all

# Validate specific environment
./enva validate --name xdxtools-core
```

### Remove Environments

```bash
# Remove environment
./enva remove my-env
```

## 🔧 Configuration

Environment configurations are defined in `src/configs/`:
- `xdxtools-core.yaml`: Core bioinformatics tools
- `xdxtools-r.yaml`: R/Bioconductor packages
- `xdxtools-snakemake.yaml`: Workflow engine
- `xdxtools-extra.yaml`: Advanced tools

## 📊 Performance

| Metric | xdxtools | enva | Improvement |
|--------|----------|------|-------------|
| Binary Size | ~50MB | 5.4MB | **89% smaller** |
| Startup Time | 2-3s | ~0.2s | **10-15x faster** |
| Memory Usage | ~100MB | ~30MB | **70% less** |
| Dependencies | ~50 | 9 | **82% reduction** |

## 🎓 Migration from xdxtools

This tool was extracted from `xdxtools-rs` with minimal changes:

1. **Same micromamba logic**: 100% code reuse
2. **Simplified CLI**: Removed xdxtools-specific options
3. **Reduced dependencies**: Removed unused crates
4. **Improved performance**: LTO, strip, optimized builds

## 📝 Command Reference

```
enva [OPTIONS] <COMMAND>

Commands:
  create    Create conda environments
  list      List conda environments
  validate  Validate environment configuration
  install   Install components in environment
  remove    Remove conda environment
  run       Run command or script in environment
  help      Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose    Enable verbose output
  -q, --quiet      Quiet mode (suppress output)
  -l, --log <LOG>  Log file path
      --dry-run    Enable dry-run mode (validate without creating)
      --json       Output in JSON format
  -h, --help       Print help
  -V, --version    Print version
```

## 🔍 Troubleshooting

### Common Issues

#### Environment creation fails
- **Error**: "Failed to create environment"
- **Solution**: Check micromamba installation, verify YAML config syntax
- **Exit code**: 1

#### Package not found
- **Error**: "Package installation failed: package not found"
- **Solution**: Verify package name, check channel availability
- **Exit code**: 1

#### Configuration file error
- **Error**: "Failed to parse YAML configuration"
- **Solution**: Validate YAML syntax, check file path
- **Exit code**: 3

### Exit Codes

- `0`: Success
- `1`: General error
- `2`: Command line argument error
- `3`: Configuration file error
- `4`: Network/download error

### Configuration Override

You can override the default package manager by setting the `ENVA_PACKAGE_MANAGER` environment variable:

```bash
# Force use of conda
ENVA_PACKAGE_MANAGER=conda enva create --core

# Force use of mamba
ENVA_PACKAGE_MANAGER=mamba enva create --core

# Force use of micromamba
ENVA_PACKAGE_MANAGER=micromamba enva create --core
```

## 🛠️ Development

```bash
# Build dev version
cargo build

# Run tests
cargo test

# Build release version
cargo build --release

# Check for warnings
cargo clippy
```

## 📜 License

MIT License - see LICENSE file for details.

## 🙏 Credits

- Built on [micromamba](https://github.com/mamba-org/micromamba) by mamba-org
- Extracted from [xdxtools-rs](https://github.com/Genomiclab/xdxtools)
