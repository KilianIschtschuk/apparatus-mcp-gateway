use axum::{
    Json, Router,
    extract::State,
    response::sse::{Event, Sse},
    routing::{get, post},
};
use dashmap::DashMap;
use futures_util::stream::Stream;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
struct Config {
    gateway_port: u16,
    mcp_servers: HashMap<String, ServerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ServerConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Option<HashMap<String, String>>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct NamespacedId {
    server_name: String,
    original_id: Value, // Could be Number or String per JSON-RPC
}

impl NamespacedId {
    // Convert to a string like "weather_service::1" for the LLM
    fn encode(&self) -> Value {
        json!(format!("{}::{}", self.server_name, self.original_id))
    }

    // Parse "weather_service::1" back into its parts
    fn decode(encoded: &Value) -> Option<Self> {
        let s = encoded.as_str()?;
        let parts: Vec<&str> = s.splitn(2, "::").collect();
        if parts.len() == 2 {
            Some(Self {
                server_name: parts[0].to_string(),
                original_id: serde_json::from_str(parts[1]).ok()?,
            })
        } else {
            None
        }
    }
}

async fn initialize_servers(config: Config) -> DashMap<String, UpstreamServer> {
    let registry = DashMap::new();

    for (name, server_cfg) in config.mcp_servers {
        match server_cfg {
            ServerConfig::Stdio { command, args, .. } => {
                // Spawn the process
                let mut child = tokio::process::Command::new(command)
                    .args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .expect("Failed to spawn");

                // Set up channels and background reader...
                // (See previous response logic)
            }
            ServerConfig::Http { url, .. } => {
                // Initialize an SSE client connection using reqwest or similar
                // This task would forward SSE events to the gateway's main loop
            }
        }
    }
    registry
}

// This tracks a request sent from the Gateway to an Upstream Server
type PendingRequests = Arc<DashMap<Value, oneshot::Sender<Value>>>;

struct UpstreamServer {
    stdin_tx: mpsc::Sender<Value>,
    // Other metadata like tool names this server supports
}

struct GatewayState {
    // Maps Request ID -> Oneshot channel to return the result to the HTTP handler
    pending_responses: PendingRequests,
    // Maps Server Name -> Server Handle
    servers: DashMap<String, UpstreamServer>,
}

async fn message_handler(
    State(state): State<Arc<GatewayState>>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, String> {
    let method = payload["method"].as_str().unwrap_or("");

    // CASE A: The client wants to see all tools available across ALL servers
    if method == "tools/list" {
        let aggregated = aggregate_tools(state).await;
        return Ok(Json(aggregated));
    }

    // CASE B: The client wants to call a specific tool
    if method == "tools/call" {
        let full_name = payload["params"]["name"].as_str().unwrap_or("");

        // Split "time_service::get_current_time" into ("time_service", "get_current_time")
        let parts: Vec<&str> = full_name.splitn(2, "::").collect();
        if parts.len() < 2 {
            return Err("Invalid tool name format. Use server::tool".into());
        }

        let target_server_name = parts[0];
        let actual_tool_name = parts[1];

        if let Some(server) = state.servers.get(target_server_name) {
            let (tx, rx) = oneshot::channel();
            let id = payload["id"].clone();
            state.pending_responses.insert(id, tx);

            // Create a modified payload with the "clean" tool name for the upstream
            let mut upstream_payload = payload.clone();
            upstream_payload["params"]["name"] = json!(actual_tool_name);

            server
                .stdin_tx
                .send(upstream_payload)
                .await
                .map_err(|_| "Upstream error")?;

            let response = rx.await.map_err(|_| "Upstream dropped request")?;
            return Ok(Json(response));
        }
    }

    Err("Method not supported or server not found".into())
}

async fn sse_handler(
    State(_state): State<Arc<GatewayState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Dummy channel to satisfy the stream type for now
    let (_tx, rx) = mpsc::channel::<Value>(1);

    let stream = ReceiverStream::new(rx)
        .map(|msg: serde_json::Value| Ok(Event::default().data(msg.to_string())));

    Sse::new(stream)
}

async fn spawn_upstream_listener(
    mut stdout_reader: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    pending_responses: PendingRequests,
) {
    while let Ok(Some(line)) = stdout_reader.next_line().await {
        if let Ok(response) = serde_json::from_str::<Value>(&line) {
            let id = response["id"].clone();

            // If we have a matching ID in our map, send it back to the waiting HTTP handler
            if let Some((_, tx)) = pending_responses.remove(&id) {
                let _ = tx.send(response);
            }
            // Otherwise, it might be a notification or log, handle accordingly
        }
    }
}

async fn aggregate_tools(state: Arc<GatewayState>) -> Value {
    let mut all_tools = Vec::new();

    for entry in state.servers.iter() {
        let server_name = entry.key();
        let server = entry.value();

        // --- STEP 1: INITIALIZE (Required by MCP Spec) ---
        let (init_tx, init_rx) = oneshot::channel();
        let init_id = json!(format!("{}_init_handshake", server_name));
        state.pending_responses.insert(init_id.clone(), init_tx);

        let _ = server
            .stdin_tx
            .send(json!({
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "rust-gateway", "version": "1.0.0" }
                }
            }))
            .await;

        // Wait for initialize response
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), init_rx).await;

        // Send initialized notification
        let _ = server
            .stdin_tx
            .send(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .await;

        // --- STEP 2: LIST TOOLS ---
        let (tx, rx) = oneshot::channel();
        let request_id = json!(format!("{}_list", server_name));
        state.pending_responses.insert(request_id.clone(), tx);

        let _ = server
            .stdin_tx
            .send(json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/list"
            }))
            .await;

        if let Ok(Ok(response)) = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await
        {
            if let Some(tools) = response["result"]["tools"].as_array() {
                for tool in tools {
                    let mut tool_clone = tool.clone();
                    if let Some(name) = tool_clone["name"].as_str() {
                        tool_clone["name"] = json!(format!("{}::{}", server_name, name));
                    }
                    all_tools.push(tool_clone);
                }
            }
        }
    }

    json!({ "jsonrpc": "2.0", "result": { "tools": all_tools } })
}

#[tokio::main]
async fn main() {
    // 1. Load Declarative Config (Simplified for example)
    let config_str = std::fs::read_to_string("config.yaml").expect("Need config.yaml");
    let config: Config = serde_yaml::from_str(&config_str).expect("Invalid YAML");

    // 2. Initialize State correctly (Fixes E0560)
    let state = Arc::new(GatewayState {
        pending_responses: Arc::new(DashMap::new()),
        servers: DashMap::new(),
    });

    // 3. Spawn Upstream Servers from Config
    for (name, srv_cfg) in config.mcp_servers {
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Value>(32);

        match srv_cfg {
            ServerConfig::Stdio { command, args, .. } => {
                let mut child = tokio::process::Command::new(command)
                    .args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .expect("Failed to spawn process");

                let mut child_stdin = child.stdin.take().unwrap();
                let child_stdout = child.stdout.take().unwrap();
                let pending_res = Arc::clone(&state.pending_responses);

                // Task: Forward Gateway -> Process Stdin
                tokio::spawn(async move {
                    while let Some(cmd) = stdin_rx.recv().await {
                        let _ = tokio::io::AsyncWriteExt::write_all(
                            &mut child_stdin,
                            format!("{}\n", cmd).as_bytes(),
                        )
                        .await;
                    }
                });

                // Task: Forward Process Stdout -> Gateway (The "Reader")
                let mut reader = BufReader::new(child_stdout);
                let mut lines = reader.lines();

                tokio::spawn(async move {
                    // 2. Correct the while let pattern to match Result<Option<String>, Error>
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Ok(res) = serde_json::from_str::<Value>(&line) {
                            if let Some(id) = res.get("id") {
                                if let Some((_, tx)) = pending_res.remove(id) {
                                    let _ = tx.send(res);
                                }
                            }
                        }
                    }
                });
            }
            ServerConfig::Http { url, .. } => {
                // Similar logic but using reqwest and SSE events
                todo!("Implement SSE Client for remote MCP");
            }
        }

        state.servers.insert(name, UpstreamServer { stdin_tx });
    }

    // 4. Build Axum Router
    let app = Router::new()
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", config.gateway_port))
        .await
        .unwrap();
    println!("Gateway running on port {}", config.gateway_port);
    axum::serve(listener, app).await.unwrap();
}
