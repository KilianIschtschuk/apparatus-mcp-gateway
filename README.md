# Apparatus MCP Gateway

A high-performance **Model Context Protocol (MCP) Gateway** built in Rust. This gateway acts as a multiplexer, allowing LLM clients to communicate with multiple upstream MCP servers (both local Stdio processes and remote HTTP/SSE services) through a single unified endpoint.

## Features

* **Multiplexing:** Combine multiple MCP servers into one tool list.
* **Namespacing:** Automatically prefixes tools (e.g., `time_service::get_time`) to prevent conflicts between servers.
* **Transport Bridging:** Connects standard JSON-RPC HTTP requests to Stdio-based child processes.
* **Declarative Config:** Manage your tools via a simple `config.yaml` file.
* **Built with Axum & Tokio:** Fully asynchronous, safe, and lightning-fast.

## Getting Started

### Prerequisites

* [Rust](https://www.rust-lang.org/tools/install) (latest stable)
* [uv](https://github.com/astral-sh/uv) (optional, for running Python-based MCP tools)

### Installation

1. Clone the repository:
```bash
git clone https://github.com/youruser/apparatus-mcp-gateway.git
cd apparatus-mcp-gateway

```


2. Build the project:
```bash
cargo build --release

```



## Configuration

Create a `config.yaml` in the project root:

```yaml
gateway_port: 3000

mcp_servers:
  # Local Python server using uvx
  time_service:
    type: stdio
    command: "uvx"
    args: ["mcp-server-time"]

  # Local Node.js server
  filesystem:
    type: stdio
    command: "node"
    args: ["/path/to/mcp-server-filesystem/index.js", "/allowed/path"]

```

## 📖 Usage

1. **Run the Gateway:**
```bash
cargo run

```


2. **List available tools:**
```bash
curl -X POST http://localhost:3000/message \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","id":"1","method":"tools/list","params":{}}'

```


3. **Call a tool:**
```bash
curl -X POST http://localhost:3000/message \
     -H "Content-Type: application/json" \
     -d '{
       "jsonrpc": "2.0",
       "id": "2",
       "method": "tools/call",
       "params": {
         "name": "time_service::get_current_time",
         "arguments": {"timezone": "UTC"}
       }
     }'

```



## Development
**Format code:**
```bash
cargo fmt
```
**Lint code:**
```bash
cargo clippy
```
