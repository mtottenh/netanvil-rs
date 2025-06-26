# netanvil-api Implementation Guide

## Overview

The `netanvil-api` crate provides the external API server for NetAnvil-RS, exposing gRPC, REST, and WebSocket interfaces for programmatic access to the load testing system.

## Related Design Documents

- [External API Design](../../section-5-1-external-api-design.md) - Complete API specification
- [Job Scheduling System](../../section-5-2-job-scheduling-design.md) - Job management APIs
- [Distributed Coordination](../../section-4-2-distributed-coordination-design.md) - Cluster management context

## Architecture

### Service Layer Structure

```rust
use tonic::{Request, Response, Status};
use netanvil_core::LoadTest;
use netanvil_distributed::DistributedCoordinator;
use netanvil_scheduler::JobScheduler;

pub struct ApiServer {
    /// Core services
    test_service: TestManagementServiceImpl,
    cluster_service: ClusterManagementServiceImpl,
    metrics_service: MetricsServiceImpl,
    job_service: JobManagementServiceImpl,
    
    /// Shared state
    coordinator: Arc<DistributedCoordinator>,
    scheduler: Arc<JobScheduler>,
    
    /// Infrastructure
    auth: Arc<AuthService>,
    rate_limiter: Arc<RateLimiter>,
    event_bus: Arc<EventBus>,
}

impl ApiServer {
    pub async fn serve(self, addr: SocketAddr) -> Result<()> {
        // Build gRPC server
        let grpc = Server::builder()
            .layer(AuthLayer::new(self.auth.clone()))
            .layer(RateLimitLayer::new(self.rate_limiter.clone()))
            .add_service(TestManagementServiceServer::new(self.test_service))
            .add_service(ClusterManagementServiceServer::new(self.cluster_service))
            .add_service(MetricsServiceServer::new(self.metrics_service))
            .add_service(JobManagementServiceServer::new(self.job_service));
        
        // Start gRPC server
        let grpc_handle = tokio::spawn(grpc.serve(addr));
        
        // Start REST gateway on different port
        let rest_handle = tokio::spawn(
            self.start_rest_gateway(addr.port() + 1)
        );
        
        // Start WebSocket server
        let ws_handle = tokio::spawn(
            self.start_websocket_server(addr.port() + 2)
        );
        
        tokio::try_join!(grpc_handle, rest_handle, ws_handle)?;
        Ok(())
    }
}
```

### gRPC Service Implementation

```rust
#[tonic::async_trait]
impl TestManagementService for TestManagementServiceImpl {
    async fn get_test_status(
        &self,
        request: Request<GetTestStatusRequest>,
    ) -> Result<Response<TestStatus>, Status> {
        // Extract claims from request extensions (set by auth layer)
        let claims = request.extensions()
            .get::<Claims>()
            .ok_or_else(|| Status::unauthenticated("Missing auth"))?;
        
        // Get test status from coordinator
        let test_id = &request.get_ref().test_id;
        let status = self.coordinator
            .get_test_status(test_id)
            .await
            .map_err(|e| Status::not_found(format!("Test not found: {}", e)))?;
        
        // Convert to proto format
        Ok(Response::new(status.into()))
    }
    
    type StreamTestStatusStream = ReceiverStream<Result<TestStatus, Status>>;
    
    async fn stream_test_status(
        &self,
        request: Request<StreamTestStatusRequest>,
    ) -> Result<Response<Self::StreamTestStatusStream>, Status> {
        let test_id = request.get_ref().test_id.clone();
        let (tx, rx) = mpsc::channel(128);
        
        // Subscribe to test events
        let mut events = self.event_bus.subscribe_test(&test_id).await;
        
        // Spawn streaming task
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if tx.send(Ok(event.into())).await.is_err() {
                    break; // Client disconnected
                }
            }
        });
        
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
```

### REST Gateway

Using warp for the REST API with automatic gRPC forwarding:

```rust
pub fn create_rest_gateway(
    grpc_endpoint: String,
) -> impl Filter<Extract = impl Reply> {
    // Create gRPC clients
    let test_client = TestManagementServiceClient::connect(grpc_endpoint.clone());
    let cluster_client = ClusterManagementServiceClient::connect(grpc_endpoint);
    
    // Test status endpoint
    let get_test_status = warp::path!("api" / "v1" / "tests" / String)
        .and(warp::get())
        .and(with_auth())
        .and_then(move |test_id: String, auth: Auth| {
            let client = test_client.clone();
            async move {
                let request = GetTestStatusRequest { test_id };
                let response = client.get_test_status(request).await
                    .map_err(|e| warp::reject::custom(ApiError::from(e)))?;
                Ok::<_, Rejection>(warp::reply::json(&response.into_inner()))
            }
        });
    
    // WebSocket upgrade for streaming
    let ws_route = warp::path!("api" / "v1" / "tests" / String / "stream")
        .and(warp::ws())
        .and(with_auth())
        .map(|test_id: String, ws: warp::ws::Ws, auth: Auth| {
            ws.on_upgrade(move |socket| handle_test_stream(socket, test_id, auth))
        });
    
    get_test_status
        .or(ws_route)
        .with(warp::cors().allow_any_origin())
}
```

### WebSocket Handler

```rust
async fn handle_test_stream(
    ws: WebSocket,
    test_id: String,
    auth: Auth,
) {
    let (mut tx, mut rx) = ws.split();
    
    // Subscribe to test events
    let mut events = EVENT_BUS.subscribe_test(&test_id).await;
    
    // Forward events to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let msg = Message::text(serde_json::to_string(&event).unwrap());
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });
    
    // Handle incoming messages (for control)
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = rx.next().await {
            if let Ok(text) = msg.to_str() {
                // Handle control messages
                match serde_json::from_str::<ControlMessage>(text) {
                    Ok(ControlMessage::ModifyRate { rps }) => {
                        // Forward to coordinator
                    }
                    _ => {}
                }
            }
        }
    });
    
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
}
```

## Authentication & Authorization

### JWT-based Authentication

```rust
pub struct AuthService {
    jwt_secret: String,
    api_keys: Arc<RwLock<HashMap<String, ApiKeyInfo>>>,
}

impl AuthService {
    pub fn verify_token(&self, token: &str) -> Result<Claims, AuthError> {
        let validation = Validation::new(Algorithm::HS256);
        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
            &validation,
        )?;
        
        Ok(token_data.claims)
    }
    
    pub async fn verify_api_key(&self, key: &str) -> Result<Claims, AuthError> {
        let api_keys = self.api_keys.read().await;
        let info = api_keys.get(key)
            .ok_or(AuthError::InvalidApiKey)?;
        
        // Check if key is active
        if !info.active || info.expires_at < Utc::now() {
            return Err(AuthError::ExpiredApiKey);
        }
        
        Ok(Claims {
            sub: info.owner.clone(),
            permissions: info.permissions.clone(),
            exp: info.expires_at.timestamp() as usize,
        })
    }
}

// Tower middleware for auth
pub struct AuthLayer {
    auth_service: Arc<AuthService>,
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthMiddleware<S>;
    
    fn layer(&self, inner: S) -> Self::Service {
        AuthMiddleware {
            inner,
            auth_service: self.auth_service.clone(),
        }
    }
}
```

## Rate Limiting

Token bucket implementation per client:

```rust
pub struct RateLimiter {
    buckets: Arc<DashMap<String, TokenBucket>>,
    config: RateLimitConfig,
}

#[derive(Clone)]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
    pub burst_size: u32,
    pub cleanup_interval: Duration,
}

impl RateLimiter {
    pub async fn check_limit(&self, client_id: &str) -> Result<(), RateLimitError> {
        let mut bucket = self.buckets
            .entry(client_id.to_string())
            .or_insert_with(|| TokenBucket::new(
                self.config.requests_per_second,
                self.config.burst_size,
            ));
        
        if bucket.try_consume(1) {
            Ok(())
        } else {
            Err(RateLimitError::ExceededLimit {
                retry_after: bucket.time_until_refill(),
            })
        }
    }
    
    // Periodic cleanup of old buckets
    pub async fn cleanup_loop(&self) {
        let mut interval = tokio::time::interval(self.config.cleanup_interval);
        
        loop {
            interval.tick().await;
            
            let now = Instant::now();
            self.buckets.retain(|_, bucket| {
                now.duration_since(bucket.last_access) < Duration::from_secs(3600)
            });
        }
    }
}
```

## Event Bus for Real-time Updates

```rust
pub struct EventBus {
    test_channels: Arc<DashMap<String, Vec<mpsc::Sender<TestEvent>>>>,
    cluster_channels: Arc<RwLock<Vec<mpsc::Sender<ClusterEvent>>>>,
}

impl EventBus {
    pub async fn publish_test_event(&self, test_id: &str, event: TestEvent) {
        if let Some(channels) = self.test_channels.get(test_id) {
            // Send to all subscribers
            let mut closed = Vec::new();
            
            for (idx, tx) in channels.iter().enumerate() {
                if tx.send(event.clone()).await.is_err() {
                    closed.push(idx);
                }
            }
            
            // Remove closed channels
            if !closed.is_empty() {
                drop(channels); // Release read lock
                
                if let Some(mut channels) = self.test_channels.get_mut(test_id) {
                    for idx in closed.into_iter().rev() {
                        channels.remove(idx);
                    }
                }
            }
        }
    }
    
    pub async fn subscribe_test(&self, test_id: &str) -> mpsc::Receiver<TestEvent> {
        let (tx, rx) = mpsc::channel(256);
        
        self.test_channels
            .entry(test_id.to_string())
            .or_default()
            .push(tx);
        
        rx
    }
}
```

## Integration with Core Components

### Coordinator Integration

```rust
impl ApiServer {
    pub fn new(
        coordinator: Arc<DistributedCoordinator>,
        scheduler: Arc<JobScheduler>,
    ) -> Self {
        // Set up event forwarding from coordinator to event bus
        let event_bus = Arc::new(EventBus::new());
        let bus_clone = event_bus.clone();
        
        coordinator.on_test_event(move |test_id, event| {
            let bus = bus_clone.clone();
            tokio::spawn(async move {
                bus.publish_test_event(test_id, event).await;
            });
        });
        
        Self {
            test_service: TestManagementServiceImpl::new(coordinator.clone()),
            cluster_service: ClusterManagementServiceImpl::new(coordinator.clone()),
            metrics_service: MetricsServiceImpl::new(coordinator.clone()),
            job_service: JobManagementServiceImpl::new(scheduler.clone()),
            coordinator,
            scheduler,
            auth: Arc::new(AuthService::new()),
            rate_limiter: Arc::new(RateLimiter::new()),
            event_bus,
        }
    }
}
```

## Performance Optimizations

### Connection Pooling

```rust
pub struct GrpcConnectionPool {
    connections: Arc<DashMap<NodeId, Channel>>,
    connect_timeout: Duration,
}

impl GrpcConnectionPool {
    pub async fn get_channel(&self, node_id: &NodeId) -> Result<Channel> {
        if let Some(channel) = self.connections.get(node_id) {
            if channel.ready().await.is_ok() {
                return Ok(channel.clone());
            }
        }
        
        // Create new connection
        let endpoint = self.get_node_endpoint(node_id)?;
        let channel = Channel::from_shared(endpoint)?
            .timeout(self.connect_timeout)
            .connect()
            .await?;
        
        self.connections.insert(node_id.clone(), channel.clone());
        Ok(channel)
    }
}
```

### Response Caching

```rust
pub struct ApiCache {
    cluster_status: Cache<(), ClusterStatus>,
    node_details: Cache<NodeId, NodeDetails>,
    test_summaries: Cache<String, TestSummary>,
}

impl ApiCache {
    pub fn new() -> Self {
        Self {
            cluster_status: Cache::builder()
                .time_to_live(Duration::from_secs(5))
                .build(),
            node_details: Cache::builder()
                .time_to_live(Duration::from_secs(30))
                .max_capacity(1000)
                .build(),
            test_summaries: Cache::builder()
                .time_to_live(Duration::from_secs(60))
                .max_capacity(10000)
                .build(),
        }
    }
}
```

## Monitoring & Observability

```rust
pub struct ApiMetrics {
    request_counter: IntCounterVec,
    request_duration: HistogramVec,
    active_streams: IntGauge,
    auth_failures: IntCounter,
    rate_limit_rejections: IntCounter,
}

impl ApiMetrics {
    pub fn record_request(&self, method: &str, status: u16, duration: Duration) {
        self.request_counter
            .with_label_values(&[method, &status.to_string()])
            .inc();
        
        self.request_duration
            .with_label_values(&[method])
            .observe(duration.as_secs_f64());
    }
}
```

## Configuration

```toml
[api]
# Server configuration
grpc_port = 50051
rest_port = 8080
ws_port = 8081

# Authentication
jwt_secret = "${JWT_SECRET}"
api_key_file = "/etc/netanvil/api_keys.json"

# Rate limiting
rate_limit_rps = 100
rate_limit_burst = 200

# TLS configuration
tls_cert = "/etc/netanvil/cert.pem"
tls_key = "/etc/netanvil/key.pem"

# Monitoring
metrics_port = 9090
```

## Testing Strategy

### Integration Tests

```rust
#[tokio::test]
async fn test_api_server_lifecycle() {
    // Start test coordinator
    let coordinator = create_test_coordinator().await;
    let scheduler = create_test_scheduler().await;
    
    // Start API server
    let server = ApiServer::new(coordinator, scheduler);
    let addr = "127.0.0.1:0".parse().unwrap();
    let handle = tokio::spawn(server.serve(addr));
    
    // Wait for server to start
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Create client and test
    let client = TestManagementServiceClient::connect("http://127.0.0.1:50051").await?;
    
    // Test endpoints...
}
```

This implementation provides a robust, scalable API server with support for multiple protocols and real-time streaming capabilities.