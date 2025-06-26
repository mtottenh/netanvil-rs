# External API Design

## 1. Introduction

The External API provides programmatic access to the NetAnvil-RS distributed load testing system. It enables monitoring, control, and management of load tests, cluster health, and system configuration through a well-defined interface.

### Key Requirements

- **Real-time Monitoring**: Stream metrics and status updates during test execution
- **Test Control**: Start, stop, pause, and modify tests in progress
- **Cluster Management**: View and manage cluster membership and health
- **Resource Management**: Monitor resource utilization and saturation
- **Security**: Authentication, authorization, and audit logging
- **Performance**: Minimal overhead on the load testing system

## 2. API Architecture

### 2.1 Protocol Selection

We use **gRPC** with HTTP/2 as the primary protocol for the following reasons:

1. **Bi-directional Streaming**: Essential for real-time metrics and updates
2. **Efficiency**: Binary protocol with low overhead
3. **Type Safety**: Strong typing with protobuf definitions
4. **Language Support**: Client libraries for multiple languages
5. **HTTP/2**: Multiplexing and flow control

Additionally, we provide a **REST API** gateway for simpler integrations:
- JSON over HTTP/1.1
- WebSocket support for streaming
- OpenAPI specification

### 2.2 Service Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    External Clients                          │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  gRPC Clients          REST Clients         WebSocket       │
│       │                     │                    │          │
│       ▼                     ▼                    ▼          │
│  ┌─────────┐          ┌──────────┐       ┌──────────┐     │
│  │  gRPC   │          │   REST   │       │WebSocket │     │
│  │ Server  │◄─────────┤ Gateway  │       │ Handler  │     │
│  └────┬────┘          └──────────┘       └────┬─────┘     │
│       │                                        │           │
│       └────────────────┬───────────────────────┘           │
│                        ▼                                    │
│               ┌─────────────────┐                          │
│               │   API Service   │                          │
│               │     Core        │                          │
│               └────────┬────────┘                          │
│                        │                                    │
│      ┌─────────────────┼─────────────────┐                │
│      ▼                 ▼                 ▼                │
│ ┌─────────┐     ┌─────────┐      ┌─────────┐            │
│ │  Test   │     │ Cluster │      │ Metrics │            │
│ │ Manager │     │ Manager │      │Aggregator│            │
│ └─────────┘     └─────────┘      └─────────┘            │
│                                                           │
│                    NetAnvil-RS Core                         │
└─────────────────────────────────────────────────────────────┘
```

## 3. API Service Definition

### 3.1 Core Services

```protobuf
syntax = "proto3";

package netanvilrs.api.v1;

import "google/protobuf/timestamp.proto";
import "google/protobuf/duration.proto";
import "google/protobuf/empty.proto";

// Test Management Service
service TestManagementService {
  // Get current test status
  rpc GetTestStatus(GetTestStatusRequest) returns (TestStatus);
  
  // Stream test status updates
  rpc StreamTestStatus(StreamTestStatusRequest) returns (stream TestStatus);
  
  // Modify running test
  rpc ModifyTest(ModifyTestRequest) returns (ModifyTestResponse);
  
  // Stop test gracefully
  rpc StopTest(StopTestRequest) returns (StopTestResponse);
  
  // Emergency stop (immediate)
  rpc EmergencyStop(EmergencyStopRequest) returns (google.protobuf.Empty);
}

// Cluster Management Service
service ClusterManagementService {
  // Get cluster status
  rpc GetClusterStatus(GetClusterStatusRequest) returns (ClusterStatus);
  
  // Stream cluster events
  rpc StreamClusterEvents(StreamClusterEventsRequest) returns (stream ClusterEvent);
  
  // Get node details
  rpc GetNodeDetails(GetNodeDetailsRequest) returns (NodeDetails);
  
  // Drain node (remove from active duty)
  rpc DrainNode(DrainNodeRequest) returns (DrainNodeResponse);
  
  // Rebalance load across cluster
  rpc RebalanceCluster(RebalanceClusterRequest) returns (RebalanceClusterResponse);
}

// Metrics Service
service MetricsService {
  // Get current metrics snapshot
  rpc GetMetrics(GetMetricsRequest) returns (MetricsSnapshot);
  
  // Stream real-time metrics
  rpc StreamMetrics(StreamMetricsRequest) returns (stream MetricUpdate);
  
  // Get historical metrics
  rpc GetHistoricalMetrics(GetHistoricalMetricsRequest) returns (HistoricalMetrics);
  
  // Export metrics in various formats
  rpc ExportMetrics(ExportMetricsRequest) returns (ExportMetricsResponse);
}

// Resource Monitoring Service
service ResourceMonitoringService {
  // Get resource utilization
  rpc GetResourceUtilization(GetResourceUtilizationRequest) returns (ResourceUtilization);
  
  // Stream saturation events
  rpc StreamSaturationEvents(StreamSaturationEventsRequest) returns (stream SaturationEvent);
  
  // Get resource predictions
  rpc GetResourcePredictions(GetResourcePredictionsRequest) returns (ResourcePredictions);
}
```

### 3.2 Data Types

```protobuf
// Test status information
message TestStatus {
  string test_id = 1;
  TestState state = 2;
  google.protobuf.Timestamp started_at = 3;
  google.protobuf.Duration elapsed = 4;
  google.protobuf.Duration remaining = 5;
  
  // Current performance
  double current_rps = 6;
  double target_rps = 7;
  LatencyMetrics latency = 8;
  double error_rate = 9;
  
  // Resource usage
  ResourceSummary resources = 10;
  
  // Test configuration
  TestConfiguration config = 11;
}

enum TestState {
  TEST_STATE_UNSPECIFIED = 0;
  TEST_STATE_INITIALIZING = 1;
  TEST_STATE_WARMING_UP = 2;
  TEST_STATE_RUNNING = 3;
  TEST_STATE_RAMPING_DOWN = 4;
  TEST_STATE_COMPLETED = 5;
  TEST_STATE_FAILED = 6;
  TEST_STATE_STOPPED = 7;
}

// Cluster status
message ClusterStatus {
  int32 total_nodes = 1;
  int32 active_nodes = 2;
  int32 coordinator_node_id = 3;
  
  repeated NodeSummary nodes = 4;
  ClusterHealth health = 5;
  
  // Aggregate capacity
  double total_capacity_rps = 6;
  double used_capacity_rps = 7;
  
  // Current epoch
  uint64 epoch_id = 8;
}

// Node information
message NodeSummary {
  string node_id = 1;
  NodeRole role = 2;
  NodeState state = 3;
  string address = 4;
  
  // Capacity and usage
  double capacity_rps = 5;
  double current_rps = 6;
  double cpu_usage = 7;
  double memory_usage = 8;
  
  // Health indicators
  google.protobuf.Timestamp last_heartbeat = 9;
  SaturationLevel saturation = 10;
}

// Real-time metric update
message MetricUpdate {
  google.protobuf.Timestamp timestamp = 1;
  string metric_name = 2;
  
  oneof value {
    double gauge_value = 3;
    uint64 counter_value = 4;
    HistogramSnapshot histogram = 5;
  }
  
  map<string, string> labels = 6;
}

// Modification request
message ModifyTestRequest {
  string test_id = 1;
  
  oneof modification {
    RateChange rate_change = 2;
    DurationExtension duration_extension = 3;
    ParameterUpdate parameter_update = 4;
  }
}

message RateChange {
  double new_target_rps = 1;
  RampStrategy ramp_strategy = 2;
  google.protobuf.Duration ramp_duration = 3;
}

enum RampStrategy {
  RAMP_STRATEGY_UNSPECIFIED = 0;
  RAMP_STRATEGY_IMMEDIATE = 1;
  RAMP_STRATEGY_LINEAR = 2;
  RAMP_STRATEGY_EXPONENTIAL = 3;
}
```

## 4. Implementation Details

### 4.1 API Service Core

```rust
use tonic::{Request, Response, Status};
use tokio_stream::wrappers::ReceiverStream;

pub struct ApiService {
    /// Reference to distributed coordinator
    coordinator: Arc<DistributedCoordinator>,
    
    /// Metrics aggregator
    metrics_aggregator: Arc<MetricsAggregator>,
    
    /// Authentication/Authorization
    auth: Arc<AuthService>,
    
    /// Rate limiter for API calls
    rate_limiter: Arc<RateLimiter>,
    
    /// Event bus for streaming
    event_bus: Arc<EventBus>,
}

#[tonic::async_trait]
impl TestManagementService for ApiService {
    async fn get_test_status(
        &self,
        request: Request<GetTestStatusRequest>,
    ) -> Result<Response<TestStatus>, Status> {
        // Check authentication
        let claims = self.auth.verify_request(&request)?;
        
        // Check rate limit
        self.rate_limiter.check(&claims.client_id).await?;
        
        // Get test status
        let test_id = &request.get_ref().test_id;
        let status = self.coordinator
            .get_test_status(test_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;
        
        Ok(Response::new(status.into()))
    }
    
    type StreamTestStatusStream = ReceiverStream<Result<TestStatus, Status>>;
    
    async fn stream_test_status(
        &self,
        request: Request<StreamTestStatusRequest>,
    ) -> Result<Response<Self::StreamTestStatusStream>, Status> {
        let claims = self.auth.verify_request(&request)?;
        let test_id = request.get_ref().test_id.clone();
        
        // Create stream
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        
        // Subscribe to test events
        let mut events = self.event_bus.subscribe_test_events(&test_id).await;
        
        // Spawn streaming task
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let Err(_) = tx.send(Ok(event.into())).await {
                    break; // Client disconnected
                }
            }
        });
        
        Ok(Response::new(ReceiverStream::new(rx)))
    }
    
    async fn modify_test(
        &self,
        request: Request<ModifyTestRequest>,
    ) -> Result<Response<ModifyTestResponse>, Status> {
        let claims = self.auth.verify_request(&request)?;
        
        // Check authorization
        if !claims.has_permission(Permission::ModifyTest) {
            return Err(Status::permission_denied("Insufficient permissions"));
        }
        
        let req = request.get_ref();
        
        // Apply modification
        match &req.modification {
            Some(modification::RateChange(change)) => {
                self.coordinator
                    .modify_test_rate(&req.test_id, change)
                    .await?
            }
            Some(modification::DurationExtension(ext)) => {
                self.coordinator
                    .extend_test_duration(&req.test_id, ext)
                    .await?
            }
            // ... other modifications
        }
        
        Ok(Response::new(ModifyTestResponse {
            success: true,
            message: "Test modified successfully".to_string(),
        }))
    }
}
```

### 4.2 Metrics Aggregation

```rust
pub struct MetricsAggregator {
    /// Local metrics registry
    local_metrics: Arc<MetricsRegistry>,
    
    /// Distributed state for remote metrics
    distributed_state: Arc<RwLock<DistributedState>>,
    
    /// Time-series storage
    time_series: Arc<TimeSeriesStore>,
    
    /// Aggregation strategies
    strategies: HashMap<String, Box<dyn AggregationStrategy>>,
}

impl MetricsAggregator {
    /// Aggregate metrics across all nodes
    pub async fn get_cluster_metrics(&self) -> MetricsSnapshot {
        let state = self.distributed_state.read().await;
        let mut aggregated = MetricsSnapshot::new();
        
        // Get all active nodes
        let nodes = state.get_active_nodes();
        
        // Aggregate metrics from each node
        for node in nodes {
            if let Some(node_metrics) = state.get_node_metrics(&node.id) {
                self.aggregate_node_metrics(&mut aggregated, node_metrics);
            }
        }
        
        aggregated
    }
    
    /// Stream real-time metric updates
    pub async fn stream_metrics(
        &self,
        filter: MetricFilter,
    ) -> impl Stream<Item = MetricUpdate> {
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        
        // Local metrics stream
        let local_stream = self.local_metrics.stream_updates();
        
        // Remote metrics via gossip
        let remote_stream = self.distributed_state
            .read()
            .await
            .stream_metric_updates();
        
        // Merge and filter streams
        tokio::spawn(async move {
            let mut combined = stream::select(local_stream, remote_stream);
            
            while let Some(update) = combined.next().await {
                if filter.matches(&update) {
                    if tx.send(update).await.is_err() {
                        break;
                    }
                }
            }
        });
        
        ReceiverStream::new(rx)
    }
}
```

### 4.3 Real-time Event Streaming

```rust
pub struct EventBus {
    /// Test event channels
    test_events: Arc<RwLock<HashMap<String, Vec<Sender<TestEvent>>>>>,
    
    /// Cluster event channels
    cluster_events: Arc<RwLock<Vec<Sender<ClusterEvent>>>>,
    
    /// Saturation event channels
    saturation_events: Arc<RwLock<Vec<Sender<SaturationEvent>>>>,
}

impl EventBus {
    /// Publish test event
    pub async fn publish_test_event(&self, test_id: &str, event: TestEvent) {
        let events = self.test_events.read().await;
        
        if let Some(subscribers) = events.get(test_id) {
            for tx in subscribers {
                let _ = tx.send(event.clone()).await;
            }
        }
    }
    
    /// Subscribe to test events
    pub async fn subscribe_test_events(&self, test_id: &str) -> Receiver<TestEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        
        self.test_events
            .write()
            .await
            .entry(test_id.to_string())
            .or_default()
            .push(tx);
        
        rx
    }
}
```

### 4.4 REST Gateway

```rust
use warp::{Filter, Reply};

pub fn create_rest_gateway(grpc_client: TestManagementServiceClient) -> impl Filter<Extract = impl Reply> {
    let get_status = warp::path!("api" / "v1" / "tests" / String)
        .and(warp::get())
        .and(with_auth())
        .and(with_grpc(grpc_client.clone()))
        .and_then(|test_id: String, auth: Auth, client: TestManagementServiceClient| async move {
            let request = GetTestStatusRequest { test_id };
            let response = client.get_test_status(request).await?;
            Ok::<_, Rejection>(warp::reply::json(&response.into_inner()))
        });
    
    let modify_test = warp::path!("api" / "v1" / "tests" / String / "modify")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_auth())
        .and(with_grpc(grpc_client.clone()))
        .and_then(|test_id: String, modification: ModificationRequest, auth: Auth, client| async move {
            // Convert REST request to gRPC
            let request = modification.to_grpc(test_id);
            let response = client.modify_test(request).await?;
            Ok::<_, Rejection>(warp::reply::json(&response.into_inner()))
        });
    
    get_status.or(modify_test)
}
```

### 4.5 WebSocket Support

```rust
use tokio_tungstenite::{WebSocketStream, accept_async};

pub async fn handle_websocket(
    ws: WebSocket,
    api_service: Arc<ApiService>,
) -> Result<()> {
    let (mut tx, mut rx) = ws.split();
    
    // Handle incoming messages
    let incoming = tokio::spawn(async move {
        while let Some(Ok(msg)) = rx.next().await {
            match msg {
                Message::Text(text) => {
                    // Parse subscription request
                    let req: SubscriptionRequest = serde_json::from_str(&text)?;
                    
                    match req.subscription_type {
                        SubscriptionType::TestStatus { test_id } => {
                            // Subscribe to test events
                            let mut events = api_service
                                .event_bus
                                .subscribe_test_events(&test_id)
                                .await;
                            
                            // Forward events to WebSocket
                            while let Some(event) = events.recv().await {
                                let msg = Message::Text(serde_json::to_string(&event)?);
                                tx.send(msg).await?;
                            }
                        }
                        // Handle other subscription types...
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        Ok::<_, Error>(())
    });
    
    incoming.await?
}
```

## 5. Security

### 5.1 Authentication

```rust
pub struct AuthService {
    /// JWT secret for token validation
    jwt_secret: String,
    
    /// API key storage
    api_keys: Arc<RwLock<HashMap<String, ApiKeyInfo>>>,
    
    /// Token cache
    token_cache: Arc<Cache<String, Claims>>,
}

impl AuthService {
    /// Verify request authentication
    pub fn verify_request<T>(&self, request: &Request<T>) -> Result<Claims, Status> {
        // Check for Bearer token
        if let Some(auth_header) = request.metadata().get("authorization") {
            let auth_str = auth_header.to_str()
                .map_err(|_| Status::unauthenticated("Invalid auth header"))?;
            
            if auth_str.starts_with("Bearer ") {
                let token = &auth_str[7..];
                return self.verify_jwt(token);
            }
        }
        
        // Check for API key
        if let Some(api_key) = request.metadata().get("x-api-key") {
            let key_str = api_key.to_str()
                .map_err(|_| Status::unauthenticated("Invalid API key"))?;
            
            return self.verify_api_key(key_str);
        }
        
        Err(Status::unauthenticated("Missing authentication"))
    }
}
```

### 5.2 Rate Limiting

```rust
pub struct RateLimiter {
    /// Token bucket per client
    buckets: Arc<RwLock<HashMap<String, TokenBucket>>>,
    
    /// Configuration
    config: RateLimitConfig,
}

impl RateLimiter {
    pub async fn check(&self, client_id: &str) -> Result<(), Status> {
        let mut buckets = self.buckets.write().await;
        
        let bucket = buckets
            .entry(client_id.to_string())
            .or_insert_with(|| TokenBucket::new(self.config.clone()));
        
        if !bucket.try_consume(1) {
            return Err(Status::resource_exhausted("Rate limit exceeded"));
        }
        
        Ok(())
    }
}
```

## 6. Performance Considerations

### 6.1 Connection Pooling

```rust
pub struct ApiConnectionPool {
    /// gRPC connections to nodes
    connections: HashMap<NodeId, Channel>,
    
    /// Connection health checker
    health_checker: HealthChecker,
}
```

### 6.2 Caching

```rust
pub struct ApiCache {
    /// Cluster status cache (short TTL)
    cluster_status: Cache<String, ClusterStatus>,
    
    /// Node details cache
    node_details: Cache<NodeId, NodeDetails>,
    
    /// Metrics cache
    metrics: Cache<MetricQuery, MetricsSnapshot>,
}
```

## 7. Client SDKs

### 7.1 Rust Client

```rust
pub struct NetAnvilClient {
    client: TestManagementServiceClient<Channel>,
    metrics_client: MetricsServiceClient<Channel>,
    cluster_client: ClusterManagementServiceClient<Channel>,
}

impl NetAnvilClient {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect()
            .await?;
        
        Ok(Self {
            client: TestManagementServiceClient::new(channel.clone()),
            metrics_client: MetricsServiceClient::new(channel.clone()),
            cluster_client: ClusterManagementServiceClient::new(channel),
        })
    }
    
    pub async fn get_test_status(&self, test_id: &str) -> Result<TestStatus> {
        let request = GetTestStatusRequest {
            test_id: test_id.to_string(),
        };
        
        let response = self.client.get_test_status(request).await?;
        Ok(response.into_inner())
    }
}
```

## 8. OpenAPI Specification

```yaml
openapi: 3.0.0
info:
  title: NetAnvil-RS API
  version: 1.0.0
  description: Distributed load testing system API

paths:
  /api/v1/tests/{testId}:
    get:
      summary: Get test status
      operationId: getTestStatus
      parameters:
        - name: testId
          in: path
          required: true
          schema:
            type: string
      responses:
        200:
          description: Test status
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/TestStatus'
                
  /api/v1/tests/{testId}/stream:
    get:
      summary: Stream test status updates
      operationId: streamTestStatus
      parameters:
        - name: testId
          in: path
          required: true
          schema:
            type: string
      responses:
        200:
          description: Server-sent events stream
          content:
            text/event-stream:
              schema:
                type: string
```

## 9. Monitoring and Observability

### 9.1 API Metrics

```rust
pub struct ApiMetrics {
    /// Request counter by endpoint
    requests: CounterVec,
    
    /// Request duration histogram
    request_duration: HistogramVec,
    
    /// Active connections gauge
    active_connections: Gauge,
    
    /// Rate limit rejections
    rate_limit_rejections: Counter,
}
```

### 9.2 Audit Logging

```rust
pub struct AuditLogger {
    /// Log sink
    sink: Box<dyn AuditSink>,
}

impl AuditLogger {
    pub async fn log_api_call(&self, event: ApiAuditEvent) {
        let entry = AuditEntry {
            timestamp: Utc::now(),
            client_id: event.client_id,
            endpoint: event.endpoint,
            method: event.method,
            request_id: event.request_id,
            response_code: event.response_code,
            duration_ms: event.duration_ms,
        };
        
        self.sink.write(entry).await;
    }
}
```

## 10. Error Handling

All API errors follow a consistent format:

```protobuf
message ApiError {
  string code = 1;          // e.g., "RATE_LIMIT_EXCEEDED"
  string message = 2;       // Human-readable message
  map<string, string> details = 3;  // Additional context
  string request_id = 4;    // For tracking
}
```

This API design provides comprehensive access to the NetAnvil-RS system while maintaining security, performance, and usability.