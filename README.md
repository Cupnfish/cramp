# CRAMP: CLI Runner and Controller for MCP Toolbox Service 🦀🤖

**An MCP (Model Control Protocol) Toolbox Service for Rust Development.**

CRAMP provides a suite of diagnostic and analysis tools exposed via MCP protocol, enabling LLM agents to autonomously understand, diagnose, and fix Rust code projects. The name comes from **Cra**b (Rust's mascot, Ferris) + **MCP**.

## Core Capabilities

- **Rust Project Diagnostics**: Detect compilation errors through integrated checks
- **Auto-Fix Management**: Retrieve and apply automatic code fixes
- **Code Navigation**: Analyze symbols and project structure
- **Testing Integration**: Verify fixes through test execution
- **State Management**: Automatic cache invalidation on code changes
- **Multi-Transport Support**: Flexible communication options

## Tool Categories

### Diagnostic Tools
- **Project Diagnostics**: Identify errors and warnings
- **Fix Retrieval**: Find available automatic fixes
- **Fix Application**: Apply selected fixes

### Analysis Tools
- **Project Structure**: Explore directory hierarchy
- **Symbol Analysis**: Inspect code symbols and documentation
- **Workspace Search**: Find symbols across project

### Testing Tools
- **Test Execution**: Run project test suite

## Cache Management Principles
- **Cache Invalidation** occurs when:
  - Running diagnostics
  - Applying fixes
  - Executing tests
- **After invalidation**:
  - Refresh diagnostics before further actions
  - Obtain new fix identifiers when needed

## Getting Started

### Installation
```bash
# Install from GitHub
cargo install --git https://github.com/cupnfish/cramp.git --bin cramp

# Or from local source
git clone https://github.com/cupnfish/cramp.git
cd cramp
cargo install --path crates/cramp
```

### Basic Usage
```bash
# Run with stdio transport
cramp stdio /path/to/project

# Run with HTTP transport
cramp stream-http /path/to/project
```

### Documentation Access
```bash
# View project documentation
cramp doc readme

# View agent interaction guidelines
cramp doc rule
```

### IDE Integration
```bash
# Install rules for VS Code
cramp rule vscode

# Install rules for Zed
cramp rule zed
```

## Configuration Options
- **Logging Control**: Adjust verbosity level
- **Timeout Settings**: Customize request and shutdown timings
- **Transport Tuning**: Configure keep-alive and state management
- **Project Parameters**: Set active project and paths

## Architecture Overview
```
Agent Interface
    │
    ▼
Transport Layer
    │
    ▼
Service Core
    │
    ▼
Toolbox Engine
    │
    ▼
Rust Ecosystem
```

## Contribution & Support
Contributions are welcome at [cupnfish/cramp](https://github.com/cupnfish/cramp). Please report issues or submit enhancements through GitHub.

## License
Dual-licensed under MIT or Apache 2.0
