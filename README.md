# enva - Rattler-First Environment Manager

enva is a standalone, rattler-first environment manager for bioinformatics workflows. It creates and maintains its own environments natively, while still discovering and interoperating with existing `conda`, `mamba`, and `micromamba` environments when needed.

## Features

- **Rattler-first by default**: native create, solve, install, run, and remove flows for rattler-managed environments
- **Compatibility aware**: discovers environments from `conda`, `mamba`, and `micromamba`, then merges same-name entries by priority
- **Adoption support**: can adopt an existing external environment into rattler ownership metadata
- **Three pre-configured environments**:
  - `xdxtools-core`
  - `xdxtools-snakemake`
  - `xdxtools-extra`
- **Operational controls**: dry-run validation, JSON output, detailed environment listing, cache cleanup

## Installation

### Download a release binary

Download the latest release asset for your platform:
- `enva-windows-x86_64.exe`
- `enva-linux-x86_64`
- `enva-macos-x86_64`
- `enva-macos-aarch64`

### Build from source

```bash
git clone <repository>
cd enva
cargo build --release
```

## Usage

### Create environments

```bash
# Create all built-in environments
./enva create --all

# Create selected built-in environments
./enva create --core
./enva create --snakemake
./enva create --extra

# Create a custom environment from YAML
./enva create --yaml ./src/configs/xdxtools-core.yaml --name xdxtools-core

# Replace an existing environment and clean rattler caches first
./enva create --yaml ./src/configs/xdxtools-core.yaml --name xdxtools-core --force --clean-cache

# Validate only
./enva --dry-run create --all
```

### List environments

```bash
# Merge same-name environments and show prefixes
./enva list

# Show owner / source / adopted-from columns
./enva list --detailed

# JSON output
./enva --json list
```

### Run commands

```bash
# Recommended syntax
./enva run xdxtools-core -- fastqc --version

# Equivalent flag-based syntax
./enva run --name xdxtools-core --command "fastqc --version"

# Explicit prefix
./enva run --prefix /path/to/env -- fastqc --version
```

### Install packages

```bash
# Install multiple packages
./enva install --name xdxtools-core fastqc multiqc

# Comma-separated input is also accepted
./enva install --name xdxtools-core fastqc,multiqc

# Mixed-channel specs are accepted in the same command
./enva install --name xdxtools-core conda-forge::jq,bioconda::seqtk
```

### Adopt or remove environments

```bash
# Adopt an existing environment by name or prefix
./enva adopt --name xdxtools-core
./enva adopt --prefix /path/to/external/env

# Remove an environment
./enva remove xdxtools-core
```

### Validate configuration

```bash
./enva validate --all
./enva validate --name xdxtools-core
```

## Compatibility model

- **Primary path**: rattler-managed environments
- **Secondary path**: adopted or external environments discovered from `conda`, `mamba`, or `micromamba`
- `ENVA_PACKAGE_MANAGER` is a compatibility hint for choosing which secondary package manager to inspect first
- `ENVA_BACKEND=cli` is an expert-only compatibility mode; the normal default remains `rattler`
- Rattler ownership metadata is stored in `conda-meta/enva-rattler.json`; when `enva` delegates install or remove operations to `micromamba`, `mamba`, or `conda`, that marker is temporarily stashed so libmamba-based tooling does not parse it as a package record

Examples:

```bash
# Prefer a specific compatibility package manager when listing/running in CLI mode
ENVA_PACKAGE_MANAGER=conda ENVA_BACKEND=cli enva run xdxtools-core -- fastqc --version

# Force explicit compatibility mode for troubleshooting
ENVA_BACKEND=cli enva list --detailed
```

## Testing

The e2e workflow covers:

- `xdxtools-core`, `xdxtools-snakemake`, and `xdxtools-extra`: create, list, validate, install extra packages, run smoke commands, and remove
- Multi-package mixed-source installs through one command, including specs like `conda-forge::jq,bioconda::seqtk`
- Adopted `micromamba` environments: adopt into rattler ownership, install extra packages through the compatibility layer, run commands, and remove through the helper package manager
- Same-name replacement under an active `CONDA_PREFIX`, ensuring the active root prefix is preferred during `create --force`

## Limitations

- `pip:` subsections inside environment YAML files are intentionally rejected by the rattler backend
- If multiple accessible environments share the same name, `enva` prioritizes rattler-owned prefixes and may ask you to disambiguate with `--prefix`

## Benchmarking

```bash
# Build the benchmark helper
cargo build --bin enva-bench

# Benchmark the default rattler-first run path
cargo run --bin enva-bench -- --env-name xdxtools-core --command "true"

# Compare with an explicit compatibility package manager
cargo run --bin enva-bench -- --env-name xdxtools-core --pm micromamba --compare-native --format json
```
