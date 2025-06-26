# Client SDK and CLI Design

## 1. Introduction

The Client SDK and CLI provide user-friendly interfaces for interacting with the NetAnvil-RS distributed load testing system. The SDK offers programmatic access across multiple languages, while the CLI provides an intuitive command-line interface for operations, automation, and scripting.

### Key Requirements

- **Multi-Language Support**: SDKs for Rust, Python, Go, and JavaScript/TypeScript
- **Intuitive CLI**: Easy-to-use commands with helpful defaults and rich output
- **Job Management**: Full lifecycle control from creation to results analysis
- **Real-time Monitoring**: Stream metrics and status updates during test execution
- **Automation-Friendly**: Scriptable with JSON/YAML support and machine-readable output
- **Authentication**: Secure credential management and API key support
- **Offline Capabilities**: Job definition validation and configuration management

## 2. Architecture Overview

### 2.1 Component Structure

```
┌─────────────────────────────────────────────────────────────────┐
│                        User Applications                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│   CLI Tool           SDK Libraries          CI/CD Scripts      │
│      │                    │                      │              │
│      └────────────────────┴──────────────────────┘              │
│                           │                                      │
│                   ┌───────▼────────┐                           │
│                   │  Client Core   │                           │
│                   │   (Rust SDK)   │                           │
│                   └───────┬────────┘                           │
│                           │                                      │
│         ┌─────────────────┼─────────────────┐                  │
│         ▼                 ▼                 ▼                  │
│  ┌──────────┐     ┌──────────┐     ┌──────────┐              │
│  │   gRPC   │     │   REST   │     │WebSocket │              │
│  │  Client  │     │  Client  │     │  Client  │              │
│  └──────────┘     └──────────┘     └──────────┘              │
│                                                                │
│                    External API                                │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 SDK Architecture

```rust
/// Core client interface shared across all SDKs
pub trait NetAnvilClient {
    /// Job management
    async fn create_job(&self, definition: JobDefinition) -> Result<Job>;
    async fn get_job(&self, job_id: &str) -> Result<Job>;
    async fn list_jobs(&self, filter: JobFilter) -> Result<Vec<Job>>;
    async fn queue_job(&self, job_id: &str, options: QueueOptions) -> Result<QueuedJob>;
    async fn delete_job(&self, job_id: &str) -> Result<()>;
    
    /// Test execution
    async fn get_test_status(&self, test_id: &str) -> Result<TestStatus>;
    async fn stream_test_status(&self, test_id: &str) -> Result<impl Stream<Item = TestStatus>>;
    async fn modify_test(&self, test_id: &str, modification: TestModification) -> Result<()>;
    async fn stop_test(&self, test_id: &str, graceful: bool) -> Result<()>;
    
    /// Cluster management
    async fn get_cluster_status(&self) -> Result<ClusterStatus>;
    async fn get_node_details(&self, node_id: &str) -> Result<NodeDetails>;
    async fn drain_node(&self, node_id: &str) -> Result<()>;
    
    /// Metrics access
    async fn get_metrics(&self, query: MetricsQuery) -> Result<MetricsSnapshot>;
    async fn stream_metrics(&self, filter: MetricFilter) -> Result<impl Stream<Item = MetricUpdate>>;
    async fn export_results(&self, test_id: &str, format: ExportFormat) -> Result<Vec<u8>>;
}
```

## 3. CLI Design

### 3.1 Command Structure

```bash
netanvil [global-options] <command> [command-options] [arguments]

Global Options:
  -c, --config <file>     Configuration file path
  -p, --profile <name>    Named profile to use
  -o, --output <format>   Output format (table, json, yaml)
  -v, --verbose           Increase verbosity
  --no-color              Disable colored output
  --api-endpoint <url>    Override API endpoint

Commands:
  job         Manage job definitions
  test        Control test execution
  cluster     View and manage cluster
  results     Analyze test results
  config      Manage CLI configuration
  auth        Authentication management
```

### 3.2 Job Management Commands

```bash
# Job operations
netanvil job create <definition-file>
netanvil job list [--filter <expression>]
netanvil job get <job-id>
netanvil job update <job-id> <definition-file>
netanvil job delete <job-id>
netanvil job validate <definition-file>

# Queue management
netanvil job queue <job-id> [--priority <n>] [--at <time>]
netanvil job dequeue <job-id>
netanvil job queue-status

# Examples
netanvil job create load-test.yaml
netanvil job list --filter "tags contains 'production'"
netanvil job queue api-test --priority 10
```

### 3.3 Test Execution Commands

```bash
# Test control
netanvil test status <test-id>
netanvil test watch <test-id>
netanvil test modify <test-id> --rps <value>
netanvil test stop <test-id> [--force]
netanvil test logs <test-id> [--follow]

# Real-time monitoring
netanvil test dashboard <test-id>  # Interactive TUI dashboard

# Examples
netanvil test watch current --metrics latency,throughput
netanvil test modify current --rps 5000 --ramp linear --duration 60s
```

### 3.4 Results Analysis

```bash
# Results commands
netanvil results get <test-id>
netanvil results compare <test-id-1> <test-id-2>
netanvil results export <test-id> --format <csv|json|html>
netanvil results report <test-id> --template <name>

# Examples
netanvil results compare baseline-test new-version-test
netanvil results export api-test-42 --format csv --output results.csv
netanvil results report performance-test --template detailed
```

## 4. SDK Implementations

### 4.1 Rust SDK (Core Implementation)

```rust
use netanvil_rs_client::prelude::*;

pub struct NetAnvilClient {
    /// gRPC client
    grpc_client: GrpcClient,
    
    /// REST client for compatibility
    rest_client: Option<RestClient>,
    
    /// WebSocket manager for streaming
    ws_manager: WebSocketManager,
    
    /// Configuration
    config: ClientConfig,
}

impl NetAnvilClient {
    pub async fn new(config: ClientConfig) -> Result<Self> {
        let grpc_client = GrpcClient::connect(&config.endpoint).await?;
        
        let rest_client = if config.enable_rest_fallback {
            Some(RestClient::new(&config.endpoint)?)
        } else {
            None
        };
        
        let ws_manager = WebSocketManager::new(&config.ws_endpoint)?;
        
        Ok(Self {
            grpc_client,
            rest_client,
            ws_manager,
            config,
        })
    }
    
    /// Create a job with builder pattern
    pub fn job(&self) -> JobBuilder {
        JobBuilder::new(self)
    }
    
    /// Stream test updates with automatic reconnection
    pub async fn watch_test(&self, test_id: &str) -> Result<TestWatcher> {
        let stream = self.ws_manager.subscribe_test(test_id).await?;
        Ok(TestWatcher::new(stream))
    }
}

/// Fluent job builder
pub struct JobBuilder<'a> {
    client: &'a NetAnvilClient,
    definition: JobDefinition,
}

impl<'a> JobBuilder<'a> {
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.definition.metadata.name = name.into();
        self
    }
    
    pub fn load_test(mut self, f: impl FnOnce(&mut LoadTestConfig)) -> Self {
        f(&mut self.definition.test_config);
        self
    }
    
    pub fn schedule(mut self, schedule: Schedule) -> Self {
        self.definition.scheduling.schedule_type = schedule.into();
        self
    }
    
    pub async fn create(self) -> Result<Job> {
        self.client.create_job(self.definition).await
    }
}

/// Example usage
#[tokio::main]
async fn main() -> Result<()> {
    let client = NetAnvilClient::connect("grpc://load-test.example.com:50051").await?;
    
    // Create a job
    let job = client.job()
        .name("API Performance Test")
        .load_test(|config| {
            config.urls = vec!["https://api.example.com/v1/users".into()];
            config.initial_rps = 100;
            config.max_rps = 1000;
            config.test_duration_secs = 600;
        })
        .schedule(Schedule::recurring("0 2 * * *"))
        .create()
        .await?;
    
    println!("Created job: {}", job.id);
    
    // Queue and watch
    let queued = client.queue_job(&job.id, QueueOptions::immediate()).await?;
    
    let mut watcher = client.watch_test(&queued.test_id).await?;
    while let Some(status) = watcher.next().await {
        println!("RPS: {}, P99: {}ms", status.current_rps, status.latency.p99_ms);
    }
    
    Ok(())
}
```

### 4.2 Python SDK

```python
from netanvil_rs import NetAnvilClient, JobDefinition, LoadTest
from netanvil_rs.streaming import TestWatcher
import asyncio

class NetAnvilPythonClient:
    """Python SDK for NetAnvil-RS load testing"""
    
    def __init__(self, endpoint: str, api_key: str = None):
        self._client = self._create_client(endpoint, api_key)
        
    async def create_job(self, definition: JobDefinition) -> Job:
        """Create a new job"""
        return await self._client.create_job(definition)
    
    async def run_test(self, job_id: str, wait: bool = True) -> TestResult:
        """Queue a job and optionally wait for completion"""
        queued = await self._client.queue_job(job_id)
        
        if wait:
            return await self._wait_for_completion(queued.test_id)
        return TestResult(test_id=queued.test_id, status="queued")
    
    async def watch_test(self, test_id: str, callback=None):
        """Watch test progress with optional callback"""
        async with TestWatcher(self._client, test_id) as watcher:
            async for status in watcher:
                if callback:
                    callback(status)
                else:
                    self._default_progress_handler(status)
    
    @contextmanager
    def load_test(self, name: str):
        """Context manager for load test definition"""
        builder = LoadTestBuilder(name)
        yield builder
        return builder.build()

# Example usage
async def main():
    client = NetAnvilPythonClient("grpc://load-test.example.com:50051")
    
    # Define a load test
    with client.load_test("API Test") as test:
        test.add_endpoint("https://api.example.com/v1/users")
        test.set_rps_range(100, 1000)
        test.set_duration(minutes=10)
        test.add_success_criteria(p99_latency_ms=200, error_rate=0.001)
    
    # Create and run
    job = await client.create_job(test.to_definition())
    
    # Run with live progress
    await client.run_test(job.id, wait=True)
```

### 4.3 Go SDK

```go
package netanvilrs

import (
    "context"
    "time"
)

type Client struct {
    grpcClient *grpcClient
    config     Config
}

// NewClient creates a new NetAnvil-RS client
func NewClient(endpoint string, opts ...Option) (*Client, error) {
    config := defaultConfig()
    for _, opt := range opts {
        opt(&config)
    }
    
    grpcClient, err := newGrpcClient(endpoint, config)
    if err != nil {
        return nil, err
    }
    
    return &Client{
        grpcClient: grpcClient,
        config:     config,
    }, nil
}

// JobBuilder provides fluent job creation
type JobBuilder struct {
    client *Client
    def    *JobDefinition
}

func (c *Client) NewJob(name string) *JobBuilder {
    return &JobBuilder{
        client: c,
        def: &JobDefinition{
            Metadata: JobMetadata{
                Name:      name,
                CreatedAt: time.Now(),
            },
        },
    }
}

func (b *JobBuilder) WithLoadTest(urls []string, rps int) *JobBuilder {
    b.def.TestConfig = LoadTestConfig{
        URLs:       urls,
        InitialRPS: rps,
        MaxRPS:     rps * 10,
    }
    return b
}

func (b *JobBuilder) Create(ctx context.Context) (*Job, error) {
    return b.client.CreateJob(ctx, b.def)
}

// Example usage
func Example() {
    client, err := NewClient("load-test.example.com:50051",
        WithAPIKey("secret-key"),
        WithTimeout(30*time.Second),
    )
    if err != nil {
        log.Fatal(err)
    }
    
    // Create a job
    job, err := client.NewJob("API Load Test").
        WithLoadTest([]string{"https://api.example.com"}, 100).
        WithDuration(10*time.Minute).
        Create(context.Background())
    
    if err != nil {
        log.Fatal(err)
    }
    
    // Run and stream results
    stream, err := client.RunTestWithStream(context.Background(), job.ID)
    if err != nil {
        log.Fatal(err)
    }
    
    for status := range stream {
        log.Printf("RPS: %.0f, P99: %.2fms", status.CurrentRPS, status.Latency.P99)
    }
}
```

### 4.4 TypeScript/JavaScript SDK

```typescript
import { NetAnvilClient, JobDefinition, LoadTest } from '@netanvil-rs/client';

export class NetAnvilJSClient {
    private client: NetAnvilClient;
    
    constructor(endpoint: string, options?: ClientOptions) {
        this.client = new NetAnvilClient(endpoint, options);
    }
    
    /**
     * Create a load test job using builder pattern
     */
    job(name: string): JobBuilder {
        return new JobBuilder(this.client, name);
    }
    
    /**
     * Quick test execution
     */
    async runTest(config: QuickTestConfig): Promise<TestResult> {
        const job = await this.job('Quick Test')
            .loadTest(lt => lt
                .urls(config.urls)
                .rps(config.rps)
                .duration(config.duration)
            )
            .create();
        
        const execution = await this.client.queueJob(job.id);
        return this.waitForCompletion(execution.testId);
    }
    
    /**
     * Stream test metrics with auto-reconnect
     */
    watchTest(testId: string): AsyncIterable<TestStatus> {
        return {
            [Symbol.asyncIterator]: async function* () {
                const stream = await this.client.streamTestStatus(testId);
                for await (const status of stream) {
                    yield status;
                }
            }.bind(this)
        };
    }
}

// React hook for test monitoring
export function useTestStatus(testId: string) {
    const [status, setStatus] = useState<TestStatus | null>(null);
    const [error, setError] = useState<Error | null>(null);
    
    useEffect(() => {
        const client = new NetAnvilJSClient(process.env.NETANVIL_RS_ENDPOINT!);
        const abortController = new AbortController();
        
        (async () => {
            try {
                for await (const update of client.watchTest(testId)) {
                    if (abortController.signal.aborted) break;
                    setStatus(update);
                }
            } catch (err) {
                setError(err as Error);
            }
        })();
        
        return () => abortController.abort();
    }, [testId]);
    
    return { status, error };
}

// Example usage
async function example() {
    const client = new NetAnvilJSClient('https://load-test.example.com');
    
    // Create a scheduled job
    const job = await client.job('Nightly Performance Test')
        .loadTest(test => test
            .urls(['https://api.example.com/v1/users'])
            .rampUp(100, 1000, '5m')
            .holdFor('10m')
            .rampDown(1000, 0, '2m')
        )
        .schedule('0 2 * * *')
        .notifications(notify => notify
            .email(['ops@example.com'])
            .slack('#performance')
            .onFailure()
        )
        .create();
    
    console.log(`Created job: ${job.id}`);
}
```

## 5. CLI Implementation

### 5.1 Core CLI Structure

```rust
use clap::{Parser, Subcommand};
use netanvil_rs_client::NetAnvilClient;

#[derive(Parser)]
#[clap(name = "netanvil", version, about = "NetAnvil-RS Load Testing CLI")]
struct Cli {
    /// Configuration file path
    #[clap(short, long, global = true)]
    config: Option<PathBuf>,
    
    /// Output format
    #[clap(short, long, global = true, default_value = "table")]
    output: OutputFormat,
    
    /// Disable colored output
    #[clap(long, global = true)]
    no_color: bool,
    
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage job definitions
    Job {
        #[clap(subcommand)]
        command: JobCommands,
    },
    
    /// Control test execution
    Test {
        #[clap(subcommand)]
        command: TestCommands,
    },
    
    /// View and manage cluster
    Cluster {
        #[clap(subcommand)]
        command: ClusterCommands,
    },
    
    /// Analyze test results
    Results {
        #[clap(subcommand)]
        command: ResultsCommands,
    },
}

/// Interactive TUI dashboard
pub struct Dashboard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    app_state: AppState,
    event_handler: EventHandler,
}

impl Dashboard {
    pub async fn run(&mut self, test_id: String) -> Result<()> {
        // Set up terminal
        enable_raw_mode()?;
        
        // Create status stream
        let mut status_stream = self.client.stream_test_status(&test_id).await?;
        
        loop {
            // Draw UI
            self.terminal.draw(|f| self.render(f))?;
            
            // Handle events
            if let Some(event) = self.event_handler.next().await {
                match event {
                    Event::Key(key) => {
                        if self.handle_key(key)? {
                            break;
                        }
                    }
                    Event::StatusUpdate(status) => {
                        self.app_state.update_status(status);
                    }
                }
            }
        }
        
        disable_raw_mode()?;
        Ok(())
    }
    
    fn render(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),    // Header
                Constraint::Min(10),      // Main content
                Constraint::Length(3),    // Footer
            ])
            .split(frame.size());
        
        // Render header
        self.render_header(frame, chunks[0]);
        
        // Render metrics
        self.render_metrics(frame, chunks[1]);
        
        // Render footer
        self.render_footer(frame, chunks[2]);
    }
}
```

### 5.2 Configuration Management

```rust
/// CLI configuration file
#[derive(Debug, Serialize, Deserialize)]
pub struct CliConfig {
    /// Named profiles
    profiles: HashMap<String, Profile>,
    
    /// Default profile
    default_profile: String,
    
    /// Global settings
    settings: GlobalSettings,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Profile {
    /// API endpoint
    endpoint: String,
    
    /// Authentication
    auth: AuthConfig,
    
    /// Default output format
    output_format: OutputFormat,
    
    /// TLS configuration
    tls: Option<TlsConfig>,
}

impl CliConfig {
    /// Load from standard locations
    pub fn load() -> Result<Self> {
        let config_path = Self::find_config_file()?;
        let contents = std::fs::read_to_string(config_path)?;
        toml::from_str(&contents)
    }
    
    /// Find config file in standard locations
    fn find_config_file() -> Result<PathBuf> {
        // Check in order:
        // 1. NETANVIL_CONFIG environment variable
        // 2. ./netanvil.toml
        // 3. ~/.config/netanvil/config.toml
        // 4. /etc/netanvil/config.toml
        
        if let Ok(path) = env::var("NETANVIL_CONFIG") {
            return Ok(PathBuf::from(path));
        }
        
        for path in &[
            PathBuf::from("netanvil.toml"),
            dirs::config_dir().unwrap().join("netanvil/config.toml"),
            PathBuf::from("/etc/netanvil/config.toml"),
        ] {
            if path.exists() {
                return Ok(path.clone());
            }
        }
        
        Err(Error::ConfigNotFound)
    }
}
```

### 5.3 Output Formatting

```rust
/// Output formatter trait
pub trait OutputFormatter: Send + Sync {
    fn format_job(&self, job: &Job) -> String;
    fn format_test_status(&self, status: &TestStatus) -> String;
    fn format_cluster_status(&self, cluster: &ClusterStatus) -> String;
    fn format_results(&self, results: &TestResults) -> String;
}

/// Table formatter for human-readable output
pub struct TableFormatter {
    style: TableStyle,
}

impl OutputFormatter for TableFormatter {
    fn format_test_status(&self, status: &TestStatus) -> String {
        let mut table = Table::new();
        table.set_style(self.style);
        
        table.add_row(row!["Test ID", status.test_id]);
        table.add_row(row!["State", format_state(&status.state)]);
        table.add_row(row!["Current RPS", format!("{:.0}", status.current_rps)]);
        table.add_row(row!["Target RPS", format!("{:.0}", status.target_rps)]);
        table.add_row(row!["P50 Latency", format!("{:.2}ms", status.latency.p50_ms)]);
        table.add_row(row!["P99 Latency", format!("{:.2}ms", status.latency.p99_ms)]);
        table.add_row(row!["Error Rate", format!("{:.2}%", status.error_rate * 100.0)]);
        
        table.to_string()
    }
}

/// JSON formatter for machine-readable output
pub struct JsonFormatter {
    pretty: bool,
}

impl OutputFormatter for JsonFormatter {
    fn format_test_status(&self, status: &TestStatus) -> String {
        if self.pretty {
            serde_json::to_string_pretty(status).unwrap()
        } else {
            serde_json::to_string(status).unwrap()
        }
    }
}
```

## 6. Authentication and Security

### 6.1 Credential Management

```rust
/// Secure credential storage
pub struct CredentialStore {
    backend: Box<dyn SecretBackend>,
}

pub trait SecretBackend: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// OS keychain backend
pub struct KeychainBackend;

impl SecretBackend for KeychainBackend {
    fn get(&self, key: &str) -> Result<Option<String>> {
        keyring::Entry::new("netanvil-rs", key)
            .get_password()
            .map(Some)
            .or_else(|e| match e {
                keyring::Error::NoEntry => Ok(None),
                _ => Err(e.into()),
            })
    }
}

/// Encrypted file backend for environments without keychain
pub struct EncryptedFileBackend {
    path: PathBuf,
    cipher: Aes256Gcm,
}
```

### 6.2 Authentication Flow

```rust
impl CliAuth {
    /// Interactive login
    pub async fn login(&self) -> Result<()> {
        println!("Logging in to NetAnvil-RS...");
        
        // Get credentials
        let username = self.prompt_username()?;
        let password = self.prompt_password()?;
        
        // Authenticate
        let token = self.client
            .authenticate(&username, &password)
            .await?;
        
        // Store token
        self.credential_store.set("api_token", &token)?;
        
        println!("✓ Successfully logged in!");
        Ok(())
    }
    
    /// Get valid authentication token
    pub async fn get_token(&self) -> Result<String> {
        // Check for environment variable
        if let Ok(token) = env::var("NETANVIL_API_TOKEN") {
            return Ok(token);
        }
        
        // Check credential store
        if let Some(token) = self.credential_store.get("api_token")? {
            // Validate token
            if self.validate_token(&token).await? {
                return Ok(token);
            }
        }
        
        // Need to login
        self.login().await?;
        self.credential_store.get("api_token")?
            .ok_or(Error::AuthenticationFailed)
    }
}
```

## 7. Advanced Features

### 7.1 Job Templates

```yaml
# templates/api-load-test.yaml
apiVersion: netanvil-rs/v1
kind: JobTemplate
metadata:
  name: api-load-test
  description: Standard API load test template
spec:
  parameters:
    - name: target_url
      type: string
      required: true
      description: Target API endpoint
    - name: max_rps
      type: integer
      default: 1000
      description: Maximum requests per second
    - name: duration_minutes
      type: integer
      default: 10
      description: Test duration in minutes
  
  template:
    test_config:
      urls: ["{{ .target_url }}"]
      initial_rps: 100
      max_rps: {{ .max_rps }}
      test_duration_secs: {{ mul .duration_minutes 60 }}
      ramp_up_secs: 60
      
    success_criteria:
      max_p99_latency_ms: 200
      max_error_rate: 0.001
      
    notifications:
      email:
        recipients: ["{{ .email | default \"ops@example.com\" }}"]
```

### 7.2 Pipeline Integration

```bash
# GitLab CI example
load-test:
  stage: performance
  script:
    - netanvil auth login --token $NETANVIL_API_TOKEN
    - |
      TEST_ID=$(netanvil job create api-test.yaml --wait | jq -r '.test_id')
      netanvil test watch $TEST_ID --timeout 15m
      netanvil results export $TEST_ID --format junit --output results.xml
  artifacts:
    reports:
      junit: results.xml
  only:
    - main
```

### 7.3 Scripting Support

```python
#!/usr/bin/env python3
# Automated performance regression detection

import subprocess
import json
import sys

def run_comparison_test(baseline_id, candidate_id):
    """Compare two test runs for regression"""
    
    # Get baseline results
    baseline = json.loads(
        subprocess.check_output(['netanvil', 'results', 'get', baseline_id, '-o', 'json'])
    )
    
    # Run candidate test
    result = subprocess.run(
        ['netanvil', 'job', 'queue', candidate_id, '--wait', '-o', 'json'],
        capture_output=True,
        text=True
    )
    
    if result.returncode != 0:
        print(f"Test failed: {result.stderr}")
        return False
    
    candidate = json.loads(result.stdout)
    
    # Compare results
    baseline_p99 = baseline['latency']['p99']
    candidate_p99 = candidate['latency']['p99']
    
    regression = (candidate_p99 - baseline_p99) / baseline_p99
    
    if regression > 0.1:  # 10% regression threshold
        print(f"Performance regression detected: {regression:.1%} increase in P99 latency")
        return False
    
    print(f"No regression detected. P99 change: {regression:+.1%}")
    return True

if __name__ == '__main__':
    sys.exit(0 if run_comparison_test(sys.argv[1], sys.argv[2]) else 1)
```

## 8. Error Handling and Diagnostics

### 8.1 Detailed Error Messages

```rust
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Connection failed: {message}")]
    ConnectionError {
        message: String,
        #[source]
        cause: Box<dyn std::error::Error + Send + Sync>,
        suggestions: Vec<String>,
    },
    
    #[error("Job validation failed: {details}")]
    ValidationError {
        details: String,
        fields: Vec<ValidationIssue>,
    },
    
    #[error("Test {test_id} failed: {reason}")]
    TestFailure {
        test_id: String,
        reason: String,
        logs_location: Option<String>,
    },
}

impl CliError {
    /// Get user-friendly error output
    pub fn format_for_terminal(&self, verbose: bool) -> String {
        let mut output = format!("Error: {}\n", self);
        
        if let Some(suggestions) = self.suggestions() {
            output.push_str("\nSuggestions:\n");
            for suggestion in suggestions {
                output.push_str(&format!("  • {}\n", suggestion));
            }
        }
        
        if verbose {
            output.push_str(&format!("\nDebug info:\n{:?}\n", self));
        }
        
        output
    }
}
```

### 8.2 Debug Mode

```rust
/// Debug command for troubleshooting
#[derive(Parser)]
struct DebugCommand {
    #[clap(subcommand)]
    command: DebugSubcommands,
}

#[derive(Subcommand)]
enum DebugSubcommands {
    /// Test connectivity to API
    Connectivity,
    
    /// Validate configuration
    Config,
    
    /// Show client version and capabilities
    Info,
    
    /// Collect diagnostic bundle
    Bundle {
        #[clap(short, long)]
        output: PathBuf,
    },
}

impl DebugCommand {
    pub async fn execute(&self, client: &NetAnvilClient) -> Result<()> {
        match &self.command {
            DebugSubcommands::Connectivity => {
                println!("Testing connectivity...");
                
                // Test gRPC
                match client.ping().await {
                    Ok(latency) => println!("✓ gRPC: {}ms", latency.as_millis()),
                    Err(e) => println!("✗ gRPC: {}", e),
                }
                
                // Test REST
                match client.rest_ping().await {
                    Ok(latency) => println!("✓ REST: {}ms", latency.as_millis()),
                    Err(e) => println!("✗ REST: {}", e),
                }
                
                // Test WebSocket
                match client.ws_ping().await {
                    Ok(latency) => println!("✓ WebSocket: {}ms", latency.as_millis()),
                    Err(e) => println!("✗ WebSocket: {}", e),
                }
            }
            // ... other debug commands
        }
        Ok(())
    }
}
```

## 9. Plugin System

### 9.1 Plugin Interface

```rust
/// Plugin trait for extending CLI functionality
pub trait CliPlugin: Send + Sync {
    /// Plugin metadata
    fn metadata(&self) -> PluginMetadata;
    
    /// Register additional commands
    fn commands(&self) -> Vec<Box<dyn PluginCommand>>;
    
    /// Hook into CLI lifecycle
    fn hooks(&self) -> PluginHooks;
}

pub struct PluginMetadata {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

pub trait PluginCommand: Send + Sync {
    /// Command name
    fn name(&self) -> &str;
    
    /// Command description
    fn description(&self) -> &str;
    
    /// Execute command
    async fn execute(&self, args: Vec<String>, context: PluginContext) -> Result<()>;
}
```

### 9.2 Plugin Example

```rust
/// Example plugin for cloud provider integration
pub struct AwsPlugin;

impl CliPlugin for AwsPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "aws-integration".to_string(),
            version: "1.0.0".to_string(),
            author: "Example Corp".to_string(),
            description: "AWS integration for NetAnvil-RS".to_string(),
        }
    }
    
    fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
        vec![
            Box::new(DeployCommand),
            Box::new(ScaleCommand),
        ]
    }
}

struct DeployCommand;

impl PluginCommand for DeployCommand {
    fn name(&self) -> &str {
        "aws-deploy"
    }
    
    async fn execute(&self, args: Vec<String>, ctx: PluginContext) -> Result<()> {
        // Deploy NetAnvil-RS cluster to AWS
        let deployer = AwsDeployer::new(ctx.config)?;
        deployer.deploy_cluster(args).await?;
        Ok(())
    }
}
```

## 10. Integration Examples

### 10.1 CI/CD Integration

```yaml
# GitHub Actions
name: Performance Test
on:
  pull_request:
    types: [opened, synchronize]

jobs:
  load-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Install NetAnvil CLI
        run: |
          curl -L https://releases.netanvil-rs.io/latest/netanvil-linux-amd64 -o netanvil
          chmod +x netanvil
          sudo mv netanvil /usr/local/bin/
      
      - name: Run Load Test
        env:
          NETANVIL_API_TOKEN: ${{ secrets.NETANVIL_API_TOKEN }}
        run: |
          # Create test from template
          netanvil job create tests/api-load-test.yaml \
            --param target_url=${{ env.PREVIEW_URL }} \
            --param max_rps=500 \
            -o json > job.json
          
          # Queue and wait
          JOB_ID=$(jq -r '.id' job.json)
          netanvil job queue $JOB_ID --wait --timeout 20m -o json > result.json
          
          # Check results
          P99=$(jq -r '.metrics.latency.p99' result.json)
          if (( $(echo "$P99 > 200" | bc -l) )); then
            echo "P99 latency too high: ${P99}ms"
            exit 1
          fi
          
      - name: Comment PR
        if: always()
        uses: actions/github-script@v6
        with:
          script: |
            const fs = require('fs');
            const result = JSON.parse(fs.readFileSync('result.json', 'utf8'));
            
            const comment = `## Load Test Results
            
            | Metric | Value |
            |--------|-------|
            | P50 Latency | ${result.metrics.latency.p50}ms |
            | P99 Latency | ${result.metrics.latency.p99}ms |
            | Throughput | ${result.metrics.throughput_rps} RPS |
            | Error Rate | ${(result.metrics.error_rate * 100).toFixed(2)}% |
            
            [Full Report](${result.report_url})`;
            
            github.rest.issues.createComment({
              issue_number: context.issue.number,
              owner: context.repo.owner,
              repo: context.repo.repo,
              body: comment
            });
```

This comprehensive Client SDK and CLI design provides powerful, user-friendly interfaces for interacting with the NetAnvil-RS system across multiple languages and use cases.