# enva - Rattler-First Environment Manager

enva is a standalone, rattler-first environment manager for bioinformatics workflows. It creates and maintains its own environments natively, while still discovering and interoperating with existing `conda`, `mamba`, and `micromamba` environments when needed.

## Features

- **Rattler-first by default**: native create, solve, install, run, and remove flows for rattler-managed environments
- **Compatibility aware**: discovers environments from `conda`, `mamba`, and `micromamba`; canonical aliases are deduplicated, while distinct same-name prefixes require explicit `--prefix` selection
- **Adoption support**: can adopt an existing external environment into rattler ownership metadata; rattler mutation and removal never adopt implicitly
- **Three pre-configured environments**:
  - `otter-core`
  - `otter-snakemake`
  - `otter-extra`
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
./enva create --yaml ./src/configs/otter-core.yaml --name otter-core

# Replace an existing environment and clean rattler caches first
./enva create --yaml ./src/configs/otter-core.yaml --name otter-core --force --clean-cache

# Create and immediately install extra packages
./enva create --core --with seqtk --with conda-forge::jq

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
./enva run otter-core -- fastqc --version

# Equivalent flag-based syntax
./enva run --name otter-core --command "fastqc --version"

# Explicit prefix
./enva run --prefix /path/to/env -- fastqc --version
```

### Activate or deactivate a shell

```bash
# One-time shell integration in Bash / Zsh
 eval "$(./enva shell hook bash)"

# After the hook is loaded, these behave like native shell commands
enva activate otter-core
enva deactivate

# Direct one-shot activation still works
 eval "$(./enva activate otter-core)"
 eval "$(./enva deactivate)"
```

```fish
# Fish hook
./enva shell hook fish | source

# After the hook is loaded
enva activate otter-core
enva deactivate
```

```powershell
# PowerShell hook
./enva shell hook powershell | Invoke-Expression

# After the hook is loaded
enva activate otter-core
enva deactivate
```

### Install packages

```bash
# Install multiple packages
./enva install --name otter-core fastqc multiqc

# Version constraints containing commas remain one MatchSpec argument
./enva install --name otter-core 'numpy>=1.24,<2'

# Mixed-channel specs are accepted as separate arguments
./enva install --name otter-core conda-forge::jq bioconda::seqtk
```

### Adopt or remove environments

```bash
# Adopt an existing environment by name or prefix
./enva adopt --name otter-core
./enva adopt --prefix /path/to/external/env

# Remove one or more uniquely resolved rattler-owned environments
./enva remove otter-core otter-extra

# Use an explicit prefix when a name maps to multiple physical environments
./enva remove --prefix /path/to/rattler-owned/env

# External environments must be adopted explicitly before rattler removal
./enva adopt --prefix /path/to/external/env
./enva remove --prefix /path/to/external/env
```

### Validate configuration

```bash
./enva validate --all
./enva validate --name otter-core
```

## Compatibility model

| Operation | Rattler backend | CLI compatibility backend |
|---|---|---|
| Create, cache cleanup | Native | Delegated to selected package manager |
| YAML validation | Native solve | Delegated basic validation |
| YAML validation with additional specs | Native solve | Unsupported |
| Install/remove by name or prefix | Native for rattler-owned prefixes; delegated for explicitly adopted prefixes | Delegated |
| Adopt external environment | Native | Unsupported |
| Discovery | Native registry plus compatibility discovery | Delegated |
| Run by name or prefix | Native prefix execution after ownership checks | Delegated |

Unsupported operations fail at the command boundary. `Native`, `Delegated`, `Hybrid`, and `Unsupported` support levels are defined in `BackendCapabilities` and implemented by each backend.

- **Primary path**: rattler-managed environments
- **Secondary path**: adopted or external environments discovered from `conda`, `mamba`, or `micromamba`
- `ENVA_PACKAGE_MANAGER` is a compatibility discovery preference; direct manager construction and `--pm` selection fail closed when the requested executable is unavailable
- `micromamba` is never downloaded or installed by `enva`; it must already be available in `PATH` or be configured with `ENVA_MICROMAMBA_PATH`
- `ENVA_BACKEND=cli` is an expert-only compatibility mode; the normal default remains `rattler`
- Rattler ownership metadata is stored in `conda-meta/enva-rattler.json`; when `enva` delegates install or remove operations to `micromamba`, `mamba`, or `conda`, that marker is temporarily stashed so libmamba-based tooling does not parse it as a package record

Examples:

```bash
# Prefer a specific compatibility package manager when listing/running in CLI mode
ENVA_PACKAGE_MANAGER=conda ENVA_BACKEND=cli enva run otter-core -- fastqc --version

# Use an explicitly installed micromamba outside PATH
ENVA_MICROMAMBA_PATH=/opt/micromamba/bin/micromamba ENVA_PACKAGE_MANAGER=micromamba ENVA_BACKEND=cli enva list --detailed

# Force explicit compatibility mode for troubleshooting
ENVA_BACKEND=cli enva list --detailed
```

## Testing

The e2e workflow covers:

- `otter-core`, `otter-snakemake`, and `otter-extra`: create, list, validate, install extra packages, run smoke commands, and remove
- Multi-package mixed-source installs through one command, including separate specs like `conda-forge::jq bioconda::seqtk`
- Adopted `micromamba` environments: adopt into rattler ownership, install extra packages through the compatibility layer, run commands, and remove through the helper package manager
- Same-name replacement under an active `CONDA_PREFIX`, ensuring the active root prefix is preferred during `create --force`

## Limitations

- `pip:` subsections inside environment YAML files are intentionally rejected by the rattler backend
- If multiple accessible environments share the same name, execution and mutation fail closed until an explicit `--prefix` is supplied
- External environments must be explicitly adopted before the rattler backend can install into, run in, or remove them

## Benchmarking

```bash
# Build the benchmark helper
cargo build --bin enva-bench

# Benchmark the default rattler-first run path
cargo run --bin enva-bench -- --env-name otter-core --command "true"

# Compare with an explicit compatibility package manager
cargo run --bin enva-bench -- --env-name otter-core --pm micromamba --compare-native --format json
```
