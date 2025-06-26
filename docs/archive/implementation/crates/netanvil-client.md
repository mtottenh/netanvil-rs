# netanvil-client Implementation Guide

## Overview

The `netanvil-client` crate provides the Rust SDK for programmatic access to the NetAnvil-RS load testing system. It offers a fluent API with automatic reconnection, streaming support, and multi-transport fallback capabilities.

## Related Design Documents

- [Client SDK/CLI Design](../../section-5-3-client-sdk-cli-design.md) - SDK architecture and API design
- [External API Design](../../section-5-1-external-api-design.md) - API protocols and endpoints
- [Job Scheduling System](../../section-5-2-job-scheduling-design.md) - Job management interface

## Core Architecture

### Client Structure

```rust
use tonic::transport::{Channel, Endpoint};
use tokio_tungstenite::WebSocketStream;
use netanvil_types::{JobDefinition, TestStatus, ClusterStatus};

pub struct NetAnvilClient {
    /// gRPC client for primary communication
    grpc_client: GrpcClient,
    
    /// REST client for fallback/compatibility
    rest_client: Option<RestClient>,
    
    /// WebSocket manager for real-time streaming
    ws_manager: WebSocketManager,
    
    /// Client configuration
    config: ClientConfig,
    
    /// Authentication handler
    auth: Arc<AuthHandler>,
    
    /// Connection health monitor
    health_monitor: Arc<HealthMonitor>,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Primary API endpoint
    pub endpoint: String,
    
    /// Enable REST fallback
    pub enable_rest_fallback: bool,
    
    /// Connection timeout
    pub connect_timeout: Duration,
    
    /// Request timeout
    pub request_timeout: Duration,
    
    /// Retry configuration
    pub retry_config: RetryConfig,
    
    /// TLS configuration
    pub tls_config: Option<TlsConfig>,
}

impl NetAnvilClient {
    /// Create a new client with the given endpoint
    pub async fn connect(endpoint: impl Into<String>) -> Result<Self> {
        Self::with_config(ClientConfig {
            endpoint: endpoint.into(),
            ..Default::default()
        }).await
    }
    
    /// Create a client with custom configuration
    pub async fn with_config(config: ClientConfig) -> Result<Self> {
        // Parse endpoint URL
        let url = Url::parse(&config.endpoint)?;
        
        // Create gRPC client
        let grpc_client = GrpcClient::connect(&config).await?;
        
        // Create REST client if enabled
        let rest_client = if config.enable_rest_fallback {
            Some(RestClient::new(&config)?)
        } else {
            None
        };
        
        // Create WebSocket manager
        let ws_endpoint = format!("ws://{}:{}/ws", 
            url.host_str().unwrap_or("localhost"),
            url.port().unwrap_or(8081)
        );
        let ws_manager = WebSocketManager::new(ws_endpoint)?;
        
        // Create auth handler
        let auth = Arc::new(AuthHandler::new());
        
        // Start health monitoring
        let health_monitor = Arc::new(HealthMonitor::new());
        health_monitor.clone().start_monitoring(grpc_client.channel.clone());
        
        Ok(Self {
            grpc_client,
            rest_client,
            ws_manager,
            config,
            auth,
            health_monitor,
        })
    }
}
```

### gRPC Client Implementation

```rust
pub struct GrpcClient {
    /// gRPC channel with automatic reconnection
    channel: Channel,
    
    /// Service clients
    test_client: TestManagementServiceClient<Channel>,
    job_client: JobManagementServiceClient<Channel>,
    cluster_client: ClusterManagementServiceClient<Channel>,
    metrics_client: MetricsServiceClient<Channel>,
    
    /// Request interceptor for auth
    interceptor: Arc<RequestInterceptor>,
}

impl GrpcClient {
    async fn connect(config: &ClientConfig) -> Result<Self> {
        // Build endpoint with retries and timeouts
        let endpoint = Endpoint::from_str(&config.endpoint)?
            .timeout(config.request_timeout)
            .connect_timeout(config.connect_timeout);
        
        // Add TLS if configured
        let endpoint = if let Some(tls) = &config.tls_config {
            endpoint.tls_config(tls.clone())?
        } else {
            endpoint
        };
        
        // Connect with retry
        let channel = Self::connect_with_retry(endpoint, &config.retry_config).await?;
        
        // Create interceptor
        let interceptor = Arc::new(RequestInterceptor::new());
        
        // Create service clients
        Ok(Self {
            test_client: TestManagementServiceClient::with_interceptor(
                channel.clone(),
                interceptor.clone()
            ),
            job_client: JobManagementServiceClient::with_interceptor(
                channel.clone(),
                interceptor.clone()
            ),
            cluster_client: ClusterManagementServiceClient::with_interceptor(
                channel.clone(),
                interceptor.clone()
            ),
            metrics_client: MetricsServiceClient::with_interceptor(
                channel.clone(),
                interceptor.clone()
            ),
            channel,
            interceptor,
        })
    }
    
    async fn connect_with_retry(
        endpoint: Endpoint,
        retry_config: &RetryConfig,
    ) -> Result<Channel> {
        let mut attempts = 0;
        let mut last_error;
        
        loop {
            match endpoint.connect().await {
                Ok(channel) => return Ok(channel),
                Err(e) => {
                    last_error = e;
                    attempts += 1;
                    
                    if attempts >= retry_config.max_attempts {
                        return Err(last_error.into());
                    }
                    
                    let delay = retry_config.backoff_duration(attempts);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}
```

### Fluent API Design

```rust
impl NetAnvilClient {
    /// Create a new job using builder pattern
    pub fn job(&self) -> JobBuilder {
        JobBuilder::new(self)
    }
    
    /// Access test operations
    pub fn test(&self) -> TestOperations {
        TestOperations::new(self)
    }
    
    /// Access cluster operations
    pub fn cluster(&self) -> ClusterOperations {
        ClusterOperations::new(self)
    }
    
    /// Access metrics operations
    pub fn metrics(&self) -> MetricsOperations {
        MetricsOperations::new(self)
    }
}

/// Job builder for fluent job creation
pub struct JobBuilder<'a> {
    client: &'a NetAnvilClient,
    definition: JobDefinition,
}

impl<'a> JobBuilder<'a> {
    pub fn new(client: &'a NetAnvilClient) -> Self {
        Self {
            client,
            definition: JobDefinition::default(),
        }
    }
    
    /// Set job name
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.definition.metadata.name = name.into();
        self
    }
    
    /// Set job description
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.definition.metadata.description = Some(desc.into());
        self
    }
    
    /// Configure load test
    pub fn load_test(mut self, f: impl FnOnce(&mut LoadTestConfig)) -> Self {
        f(&mut self.definition.test_config);
        self
    }
    
    /// Set scheduling
    pub fn schedule(mut self, schedule: Schedule) -> Self {
        self.definition.scheduling = schedule.into();
        self
    }
    
    /// Set resource requirements
    pub fn resources(mut self, f: impl FnOnce(&mut ResourceRequirements)) -> Self {
        f(&mut self.definition.resources);
        self
    }
    
    /// Set success criteria
    pub fn success_criteria(mut self, criteria: SuccessCriteria) -> Self {
        self.definition.success_criteria = Some(criteria);
        self
    }
    
    /// Configure notifications
    pub fn notifications(mut self, f: impl FnOnce(&mut NotificationConfig)) -> Self {
        f(&mut self.definition.notifications);
        self
    }
    
    /// Create the job
    pub async fn create(self) -> Result<Job> {
        self.client.create_job(self.definition).await
    }
    
    /// Create and immediately queue the job
    pub async fn create_and_run(self) -> Result<TestExecution> {
        let job = self.create().await?;
        self.client.queue_job(&job.id, QueueOptions::immediate()).await
    }
}
```

### WebSocket Streaming

```rust
pub struct WebSocketManager {
    endpoint: String,
    connections: Arc<DashMap<String, WebSocketConnection>>,
    reconnect_policy: ReconnectPolicy,
}

struct WebSocketConnection {
    ws: Arc<Mutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
    subscriptions: Arc<RwLock<HashMap<String, mpsc::Sender<StreamMessage>>>>,
    health: Arc<AtomicBool>,
}

impl WebSocketManager {
    /// Subscribe to test status updates
    pub async fn subscribe_test(
        &self,
        test_id: &str,
    ) -> Result<TestStatusStream> {
        let (tx, rx) = mpsc::channel(256);
        
        // Get or create connection
        let conn = self.get_or_create_connection().await?;
        
        // Send subscription request
        let sub_req = SubscriptionRequest {
            subscription_type: SubscriptionType::TestStatus {
                test_id: test_id.to_string(),
            },
        };
        
        conn.send_message(Message::Text(
            serde_json::to_string(&sub_req)?
        )).await?;
        
        // Register subscription
        conn.subscriptions.write().await.insert(
            test_id.to_string(),
            tx,
        );
        
        Ok(TestStatusStream::new(rx))
    }
    
    async fn get_or_create_connection(&self) -> Result<Arc<WebSocketConnection>> {
        let key = "primary"; // Could support multiple connections
        
        if let Some(conn) = self.connections.get(key) {
            if conn.health.load(Ordering::Relaxed) {
                return Ok(conn.clone());
            }
        }
        
        // Create new connection
        let conn = self.create_connection().await?;
        self.connections.insert(key.to_string(), conn.clone());
        
        // Start message handler
        self.start_message_handler(conn.clone());
        
        Ok(conn)
    }
    
    fn start_message_handler(&self, conn: Arc<WebSocketConnection>) {
        tokio::spawn(async move {
            loop {
                let msg = conn.receive_message().await;
                
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(stream_msg) = serde_json::from_str::<StreamMessage>(&text) {
                            // Route to appropriate subscription
                            if let Some(tx) = conn.subscriptions.read().await
                                .get(&stream_msg.subscription_id) 
                            {
                                let _ = tx.send(stream_msg).await;
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        conn.health.store(false, Ordering::Relaxed);
                        break;
                    }
                    Err(_) => {
                        conn.health.store(false, Ordering::Relaxed);
                        break;
                    }
                    _ => {}
                }
            }
        });
    }
}

/// Stream wrapper for test status updates
pub struct TestStatusStream {
    receiver: mpsc::Receiver<StreamMessage>,
}

impl Stream for TestStatusStream {
    type Item = TestStatus;
    
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(msg)) => {
                if let Ok(status) = serde_json::from_value(msg.data) {
                    Poll::Ready(Some(status))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
```

### Test Monitoring

```rust
pub struct TestWatcher {
    test_id: String,
    stream: TestStatusStream,
    handlers: Vec<Box<dyn StatusHandler>>,
}

#[async_trait]
pub trait StatusHandler: Send + Sync {
    async fn handle(&mut self, status: &TestStatus) -> Result<()>;
}

impl TestWatcher {
    pub fn new(test_id: String, stream: TestStatusStream) -> Self {
        Self {
            test_id,
            stream,
            handlers: Vec::new(),
        }
    }
    
    /// Add a status handler
    pub fn on_update(mut self, handler: impl StatusHandler + 'static) -> Self {
        self.handlers.push(Box::new(handler));
        self
    }
    
    /// Add a progress printer
    pub fn with_progress(self) -> Self {
        self.on_update(ProgressPrinter::default())
    }
    
    /// Watch until completion
    pub async fn watch(mut self) -> Result<TestResult> {
        while let Some(status) = self.stream.next().await {
            // Run handlers
            for handler in &mut self.handlers {
                handler.handle(&status).await?;
            }
            
            // Check if test completed
            match status.state {
                TestState::Completed => {
                    return Ok(TestResult::Success {
                        test_id: self.test_id,
                        metrics: status.metrics,
                    });
                }
                TestState::Failed => {
                    return Ok(TestResult::Failed {
                        test_id: self.test_id,
                        reason: status.error_message,
                    });
                }
                _ => {}
            }
        }
        
        Err(Error::StreamClosed)
    }
}

/// Default progress printer
#[derive(Default)]
struct ProgressPrinter {
    last_update: Option<Instant>,
}

#[async_trait]
impl StatusHandler for ProgressPrinter {
    async fn handle(&mut self, status: &TestStatus) -> Result<()> {
        // Rate limit updates
        if let Some(last) = self.last_update {
            if last.elapsed() < Duration::from_secs(1) {
                return Ok(());
            }
        }
        
        println!(
            "Test {}: RPS={:.0}, P50={:.1}ms, P99={:.1}ms, Errors={:.2}%",
            status.test_id,
            status.current_rps,
            status.latency.p50_ms,
            status.latency.p99_ms,
            status.error_rate * 100.0
        );
        
        self.last_update = Some(Instant::now());
        Ok(())
    }
}
```

### Authentication

```rust
pub struct AuthHandler {
    credentials: Arc<RwLock<Option<Credentials>>>,
}

#[derive(Clone)]
pub enum Credentials {
    ApiKey(String),
    Token(String),
    Certificate {
        cert: Vec<u8>,
        key: Vec<u8>,
    },
}

impl AuthHandler {
    /// Set API key authentication
    pub async fn set_api_key(&self, key: impl Into<String>) {
        *self.credentials.write().await = Some(Credentials::ApiKey(key.into()));
    }
    
    /// Set bearer token authentication
    pub async fn set_token(&self, token: impl Into<String>) {
        *self.credentials.write().await = Some(Credentials::Token(token.into()));
    }
    
    /// Login with username/password
    pub async fn login(
        &self,
        client: &NetAnvilClient,
        username: &str,
        password: &str,
    ) -> Result<()> {
        let response = client.grpc_client
            .auth_client
            .login(LoginRequest {
                username: username.to_string(),
                password: password.to_string(),
            })
            .await?;
        
        self.set_token(response.into_inner().token).await;
        Ok(())
    }
}

/// Request interceptor for authentication
struct RequestInterceptor {
    auth: Arc<AuthHandler>,
}

impl Interceptor for RequestInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        // Add authentication to request metadata
        if let Some(creds) = self.auth.credentials.blocking_read().as_ref() {
            match creds {
                Credentials::ApiKey(key) => {
                    request.metadata_mut().insert(
                        "x-api-key",
                        key.parse().map_err(|_| Status::invalid_argument("Invalid API key"))?
                    );
                }
                Credentials::Token(token) => {
                    request.metadata_mut().insert(
                        "authorization",
                        format!("Bearer {}", token).parse()
                            .map_err(|_| Status::invalid_argument("Invalid token"))?
                    );
                }
                _ => {}
            }
        }
        
        Ok(request)
    }
}
```

### Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Connection failed: {0}")]
    Connection(String),
    
    #[error("Authentication failed: {0}")]
    Auth(String),
    
    #[error("API error: {status} - {message}")]
    Api {
        status: tonic::Code,
        message: String,
    },
    
    #[error("Stream closed unexpectedly")]
    StreamClosed,
    
    #[error("Timeout after {0:?}")]
    Timeout(Duration),
}

impl From<tonic::Status> for ClientError {
    fn from(status: tonic::Status) -> Self {
        ClientError::Api {
            status: status.code(),
            message: status.message().to_string(),
        }
    }
}

/// Result type with retry support
pub struct RetryableResult<T> {
    result: Result<T>,
    can_retry: bool,
}

impl<T> RetryableResult<T> {
    pub async fn retry_with<F, Fut>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        match self.result {
            Ok(value) => Ok(value),
            Err(e) if self.can_retry => f().await,
            Err(e) => Err(e),
        }
    }
}
```

## Usage Examples

### Basic Usage

```rust
use netanvil_client::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    // Connect to NetAnvil-RS
    let client = NetAnvilClient::connect("http://localhost:50051").await?;
    
    // Set authentication
    client.auth().set_api_key("your-api-key").await;
    
    // Create a job
    let job = client.job()
        .name("API Performance Test")
        .load_test(|cfg| {
            cfg.urls = vec!["https://api.example.com/v1/users".into()];
            cfg.initial_rps = 100;
            cfg.max_rps = 1000;
            cfg.test_duration_secs = 600;
        })
        .success_criteria(SuccessCriteria {
            max_p99_latency_ms: Some(200),
            max_error_rate: Some(0.001),
            min_throughput_rps: Some(900),
        })
        .create()
        .await?;
    
    println!("Created job: {}", job.id);
    
    // Run the job
    let execution = client.queue_job(&job.id, QueueOptions::immediate()).await?;
    
    // Watch progress
    let result = client.test()
        .watch(&execution.test_id)
        .with_progress()
        .watch()
        .await?;
    
    match result {
        TestResult::Success { metrics, .. } => {
            println!("Test passed! P99: {:.1}ms", metrics.latency.p99_ms);
        }
        TestResult::Failed { reason, .. } => {
            eprintln!("Test failed: {}", reason.unwrap_or_default());
        }
    }
    
    Ok(())
}
```

### Advanced Streaming

```rust
// Custom status handler
struct MetricsCollector {
    samples: Vec<MetricsSample>,
}

#[async_trait]
impl StatusHandler for MetricsCollector {
    async fn handle(&mut self, status: &TestStatus) -> Result<()> {
        self.samples.push(MetricsSample {
            timestamp: Instant::now(),
            rps: status.current_rps,
            p50: status.latency.p50_ms,
            p99: status.latency.p99_ms,
            errors: status.error_rate,
        });
        Ok(())
    }
}

// Use with watcher
let collector = MetricsCollector::default();
let result = client.test()
    .watch(&test_id)
    .on_update(collector)
    .watch()
    .await?;
```

## Testing

### Mock Client for Testing

```rust
pub struct MockClient {
    responses: Arc<Mutex<VecDeque<MockResponse>>>,
}

impl MockClient {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
    
    pub fn expect_create_job(mut self, response: Job) -> Self {
        self.responses.lock().unwrap().push_back(
            MockResponse::CreateJob(response)
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_job_creation() {
        let mock = MockClient::new()
            .expect_create_job(Job {
                id: "test-123".into(),
                // ...
            });
        
        let job = mock.job()
            .name("Test Job")
            .create()
            .await
            .unwrap();
        
        assert_eq!(job.id, "test-123");
    }
}
```

This implementation provides a comprehensive Rust SDK with a fluent API, robust error handling, and excellent developer experience.