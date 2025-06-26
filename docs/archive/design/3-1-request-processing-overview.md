# 3.1 Request Processing Overview

## 3.1.1 Pipeline Architecture

The request processing pipeline is a crucial component of the load testing framework, responsible for creating, modifying, and executing HTTP requests. The pipeline consists of three primary stages, each with well-defined interfaces that enable flexible composition:

```
Generator → Transformer → Executor
```

![Request Processing Pipeline](https://mermaid.ink/img/pako:eNp1kk1vgzAMhv9KlHOLxCilq6bdph12qLTTpB02DsEQqyQGJaGsqvjvM6RQpJX54Mfvazs2B6mkQsj4Tp-kaTZMvDvhQIqMLUToKDzRUuUdxXTFLPnvUKPJiAWvpFIbsISbXFrNOg4vT-Ac3pLkmcGmfTc1dK59LY1oj3VXV-6xfR_PWH-qcnwDLblhd69VoZ0MgGJMllJ9OIqFYuEFOYoM6aSxfUKOcj1IaLUwNFkzWnDNdiHsdFyqXSoOx-GRRKaLUEg6EtAi2Ctz44rGUYQcC3UUfh9T4Y8cDKNE2LBIBPUj9OM4ioIojpexn_iJv_QXYR1aSr2uQ1cjf5FEy2XsL2LfD_5KczLcJXTBqGFPDJo3dAk57FP1J1xYY5pKkzYbz8dMu-GbqJtmOObSHNfmtTBNQ03rdDj97xST_4OlbLrHpv8GH8a7Ww)

### Key Characteristics:

1. **Loose Coupling**: Each component communicates through well-defined interfaces and data structures
2. **Strong Coordination**: Components share context to preserve crucial information like client IP addresses
3. **Stateful Awareness**: Components can maintain state across requests when needed (e.g., for connection pooling)
4. **Metrics Integration**: Each component provides detailed metrics for observability
5. **Composability**: Different implementations can be mixed and matched to create custom request pipelines

## 3.1.2 Data Flow and Core Types

The pipeline processes each request through a series of transformations using shared data structures:

### Request Context

The `RequestContext` is a crucial shared structure that flows through the pipeline, carrying metadata that influences each stage:

```rust
/// Context for request generation containing runtime information
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// Unique ID for this request
    pub request_id: u64,
    /// Scheduler ticket that triggered this request
    pub ticket: SchedulerTicket,
    /// Current timestamp
    pub timestamp: Instant,
    /// Whether detailed sampling is enabled for this request
    pub is_sampled: bool,
    /// Client-specific context for correlation
    pub client_context: Option<ClientContext>,
    /// Dynamic parameters that influence generation
    pub parameters: HashMap<String, String>,
}

/// Client-specific context for request correlation
#[derive(Debug, Clone)]
pub struct ClientContext {
    /// Connection ID for connection pooling
    pub connection_id: String,
    /// Current client IP address
    pub client_ip: IpAddr,
    /// Session ID for session-based tests
    pub session_id: Option<String>,
    /// How many requests have been sent on this connection
    pub requests_on_connection: u64,
}
```

### Request Specification

The `RequestSpec` is the primary data structure that evolves through the pipeline:

```rust
/// Full request specification
#[derive(Debug, Clone)]
pub struct RequestSpec {
    /// HTTP method (GET, POST, etc.)
    pub method: String,
    /// URL or URL pattern
    pub url_spec: UrlSpec,
    /// Headers to include
    pub headers: Vec<(String, String)>,
    /// Request body (if any)
    pub body: Option<RequestBody>,
    /// Simulated client IP configuration
    pub client_ip: Option<ClientIpSpec>,
    /// Request metadata for tracking
    pub metadata: RequestMetadata,
}

/// Client IP simulation specification
#[derive(Debug, Clone)]
pub enum ClientIpSpec {
    /// Single IP address
    Static(IpAddr),
    /// Range of IPs to cycle through
    Range(IpAddr, IpAddr),
    /// Random IPs from CIDR block
    RandomFromCidr(String),
    /// Session-based IP (consistent per connection)
    SessionBased,
}

/// Request metadata for tracking and correlation
#[derive(Debug, Clone, Default)]
pub struct RequestMetadata {
    /// Unique identifier for this request
    pub request_id: String,
    /// User-defined tags for request categorization
    pub tags: HashMap<String, String>,
    /// Group identifier for batch operations
    pub group_id: Option<String>,
    /// Session identifier for connection pooling correlation
    pub session_id: Option<String>,
    /// Whether correlation tracking is enabled
    pub enable_correlation: bool,
}
```

### Completed Request

After execution, the result is captured in a `CompletedRequest`:

```rust
/// Completed request with response information
#[derive(Debug, Clone)]
pub struct CompletedRequest {
    /// The original request ID
    pub request_id: u64,
    /// When request was actually sent
    pub start_time: Instant,
    /// When response was received
    pub end_time: Instant,
    /// HTTP status code returned
    pub status_code: u16,
    /// Whether an error occurred
    pub is_error: bool,
    /// Error details if any
    pub error: Option<String>,
    /// Size of the response in bytes
    pub response_size: u64,
    /// Response headers
    pub headers: HashMap<String, String>,
    /// Response body (if sampled)
    pub body: Option<Vec<u8>>,
    /// Transaction analysis from profiler (if available)
    pub transaction_analysis: Option<TransactionAnalysis>,
}
```

## 3.1.3 Client IP Coordination Between Components

One of the most critical coordination points is the handling of client IP addresses in connection pooling scenarios. This requires careful orchestration between components:

![Client IP Coordination](https://mermaid.ink/img/pako:eNp9kk9PwzAMxb9KlHO3RClsG2xXbhwQEkgcOHBJnNbq8o_aVNsE_HfspB3rYOTQOO_3bMd2DaVSEiHntX5XptkycdkIB1LkbCNCR-GJlqrYU0xXzJL_CjWajFjwSiq1AUu4KaTVbODwfA_O4SVNHhls2ldTQ-c61tKI9rPu6soNbe94wvqTyuc30JIbdn1Z5drJACjGZCnVm6NYKBY-kJPI0F4a2yfkKNeDhFYLQ5M1owXXbBfCTsel2qXiEAU3JDJdhELSiYAWwV55OFdcRFGEHAt1FP4YU-GPHAyjRNiwSAT1I_TjOIqCKE6XsZ_4ib_0F2EdWkq9rkNXKX-RRMtl7C9i3w_-SnMy3CV0wahhjwyaN3QJOTRU_Q4X1pim0qTNxvM50zZ8F3XTDMdcmuPavBSmaWhTq8Ph36SY_GdJ2XRPTf8FfqK_aQ)

### Key Coordination Mechanisms:

1. **Generator to Transformer**: 
   - Generator creates a client IP specification (static, range, CIDR, or session-based)
   - Transformer receives this specification and adapts it based on connection requirements

2. **Transformer to Executor**:
   - Transformer enhances requests with session correlation headers
   - For persistent connections, it ensures consistent client IP headers across requests

3. **Executor Connection Management**:
   - Maintains connection pools with associated client IPs
   - Ensures each connection consistently uses the same client IP
   - Maps session IDs to specific connections for correlation

## 3.1.4 Persistent Connection Implementation with Request Limits

The framework implements persistent connections with consistent client IP handling and configurable request limits through several coordinated mechanisms:

### 1. Session-Based Client IP Assignment

```rust
/// Example workflow for session-based client IP coordination
impl SessionCorrelationTransformer {
    fn transform(&self, mut spec: RequestSpec) -> RequestSpec {
        // Determine if this is a session-based request
        if let Some(ClientIpSpec::SessionBased) = spec.client_ip {
            // Get or create session ID
            let session_id = spec.metadata.session_id.clone()
                .unwrap_or_else(|| self.generate_session_id());
                
            // Store session ID in metadata for executor
            spec.metadata.session_id = Some(session_id.clone());
            
            // Get client IP for this session
            let client_ip = self.get_session_client_ip(&session_id);
            
            // Replace SessionBased with specific IP
            spec.client_ip = Some(ClientIpSpec::Static(client_ip));
        }
        
        spec
    }
    
    // Maintains a consistent client IP per session
    fn get_session_client_ip(&self, session_id: &str) -> IpAddr {
        if let Some(ip) = self.session_ips.get(session_id) {
            // Return existing IP for this session
            *ip
        } else {
            // Generate new IP for this session
            let ip = self.generate_client_ip();
            self.session_ips.insert(session_id.to_string(), ip);
            ip
        }
    }
}
```

### 2. Connection Pool with IP Preservation and Request Limits

```rust
/// Connection pool with client IP preservation and request limits
pub struct ConnectionPool {
    // Maps connection IDs to connections
    connections: DashMap<String, PooledConnection>,
    // Maps session IDs to connection IDs
    session_connections: DashMap<String, String>,
    // Connection lifecycle configuration
    lifecycle_config: ConnectionLifecycleConfig,
    // RNG for random request limits
    rng: Arc<Mutex<SmallRng>>,
    
    // Get or create a connection for a session with recycling awareness
    async fn get_connection_for_session(&self, session_id: &str) -> PooledConnection {
        // Check if session already has a connection
        if let Some(conn_id) = self.session_connections.get(session_id) {
            if let Some(conn) = self.connections.get(&conn_id) {
                // Check if connection has reached its request limit
                if !conn.should_recycle() {
                    // Return existing connection
                    return conn.clone();
                }
                
                // Connection needs recycling - reached request limit
                return self.recycle_connection(&conn_id.clone(), Some(session_id)).await;
            }
        }
        
        // Create new connection for this session with request limit
        let conn = self.create_connection_with_limit(None);
        self.session_connections.insert(session_id.to_string(), conn.id.clone());
        conn
    }
    
    // Create a new connection with request limit
    fn create_connection_with_limit(&self, client_ip: Option<IpAddr>) -> PooledConnection {
        // Determine request limit based on configuration
        let request_limit = match self.lifecycle_config.max_requests_per_connection {
            MaxRequestsSpec::Unlimited => u32::MAX,
            MaxRequestsSpec::Fixed(limit) => limit,
            MaxRequestsSpec::Range(min, max) => {
                let mut rng = self.rng.lock().unwrap();
                rng.gen_range(min..=max)
            }
        };
        
        // Create connection with the determined limit
        let client_ip = client_ip.unwrap_or_else(|| self.generate_random_ip());
        
        PooledConnection {
            id: Uuid::new_v4().to_string(),
            client_ip,
            session_id: None,
            max_requests: request_limit,
            requests_executed: AtomicU32::new(0),
            creation_time: Instant::now(),
            // Other initialization...
        }
    }
    
    // Recycle a connection while preserving client IP if configured
    async fn recycle_connection(&self, conn_id: &str, session_id: Option<&str>) -> PooledConnection {
        // Remove old connection from pool
        let old_conn = self.connections.remove(conn_id);
        
        // Get client IP to preserve if configured
        let client_ip = if self.lifecycle_config.preserve_client_ip_on_recycle {
            old_conn.map(|(_, conn)| conn.client_ip)
        } else {
            None
        };
        
        // Create new connection (with new ID) but same IP if preserved
        let new_conn = self.create_connection_with_limit(client_ip);
        
        // Update session mapping if needed
        if let Some(sid) = session_id {
            self.session_connections.insert(sid.to_string(), new_conn.id.clone());
        }
        
        // Add new connection to pool
        self.connections.insert(new_conn.id.clone(), new_conn.clone());
        
        new_conn
    }
}

/// Pooled connection that preserves client IP and tracks request limits
pub struct PooledConnection {
    pub id: String,
    pub client_ip: IpAddr,
    pub session_id: Option<String>,
    /// Maximum requests allowed on this connection before recycling
    max_requests: u32,
    /// Number of requests executed on this connection
    requests_executed: AtomicU32,
    /// Creation time for lifetime tracking
    creation_time: Instant,
    /// Connection lifecycle configuration reference
    lifecycle_config: Arc<ConnectionLifecycleConfig>,
    // Connection-specific state...
    
    // Check if this connection should be recycled
    fn should_recycle(&self) -> bool {
        // Check request count limit
        let request_count = self.requests_executed.load(Ordering::Relaxed);
        let count_exceeded = request_count >= self.max_requests;
        
        // Check connection lifetime limit if configured
        let lifetime_exceeded = if let Some(max_lifetime_ms) = self.lifecycle_config.max_connection_lifetime_ms {
            self.creation_time.elapsed().as_millis() > max_lifetime_ms as u128
        } else {
            false
        };
        
        count_exceeded || lifetime_exceeded
    }
    
    // Execute request while preserving client IP and tracking request count
    async fn execute(&self, mut request: Request) -> Result<Response, Error> {
        // Check if connection should be recycled
        if self.should_recycle() {
            return Err(Error::ConnectionRecycled);
        }
        
        // Ensure client IP header is set correctly
        request.headers_mut().insert(
            "X-Forwarded-For", 
            HeaderValue::from_str(&self.client_ip.to_string())?
        );
        
        // Execute request
        let response = self.client.execute(request).await;
        
        // Increment request counter after execution
        self.requests_executed.fetch_add(1, Ordering::Relaxed);
        
        response
    }
}
```

### 3. Executor Client IP Integration with Connection Recycling

```rust
/// Connection pooling executor with request limit awareness
impl ConnectionPoolExecutor {
    async fn execute(&self, spec: RequestSpec, context: &RequestContext) -> Result<CompletedRequest, Error> {
        // Determine connection to use
        let mut connection = if let Some(session_id) = &spec.metadata.session_id {
            // Get connection for this session (handles recycling internally)
            self.pool.get_connection_for_session(session_id).await
        } else if let Some(client_context) = &context.client_context {
            // Try to use existing connection from context
            let conn = self.pool.get_connection_by_id(&client_context.connection_id).await;
            
            // Check if connection needed recycling
            if conn.should_recycle() {
                // Connection reached request limit, get a fresh one
                let session_id = client_context.session_id.as_deref();
                self.pool.recycle_connection(&client_context.connection_id, session_id).await
            } else {
                conn
            }
        } else {
            // Get random connection
            self.pool.get_connection().await
        };
        
        // Try to execute request using connection
        match connection.execute(self.build_request(&spec, &connection)?).await {
            Ok(response) => {
                // Process successful response
                // ...
                Ok(completed_request)
            },
            Err(Error::ConnectionRecycled) => {
                // Connection was recycled during execution, retry with new connection
                let session_id = spec.metadata.session_id.as_deref();
                connection = self.pool.recycle_connection(&connection.id, session_id).await;
                
                // Retry with new connection
                let response = connection.execute(self.build_request(&spec, &connection)?).await?;
                // Process response
                // ...
                Ok(completed_request)
            },
            Err(e) => Err(e),
        }
    }
}
```

## 3.1.5 Coordination for HTTP/2 Multiplexing

HTTP/2 adds complexity because multiple requests share a single connection. The framework handles this by:

1. **Connection-Based IP Assignment**:
   - All requests on the same HTTP/2 connection use the same client IP
   - The executor manages stream IDs while preserving client IP consistency

2. **Session Grouping**:
   - Requests from the same session are routed to the same HTTP/2 connection
   - Session correlation headers are consistent across streams

3. **Metadata Propagation**:
   - Connection ID and client IP are included in `ClientContext`
   - Stream IDs and HTTP/2 settings are managed transparently

```rust
/// HTTP/2 connection management
impl Http2Executor {
    async fn execute(&self, spec: RequestSpec, context: &RequestContext) -> Result<CompletedRequest, Error> {
        // Get appropriate connection for this request
        let connection = self.get_or_create_connection(&spec).await?;
        
        // Build and execute request on this connection
        // All requests on this connection use the same client IP
        let response = connection.send_request(self.build_request(&spec)?).await?;
        
        // Process response...
    }
    
    fn build_request(&self, spec: &RequestSpec) -> Result<Request<Body>, Error> {
        // Build request...
        
        // Ensure client IP header is consistent with connection
        request.headers_mut().insert(
            "X-Forwarded-For",
            HeaderValue::from_str(&connection.client_ip.to_string())?
        );
        
        Ok(request)
    }
}
```

## 3.1.6 Connection Lifecycle Management

A critical aspect of realistic load testing is properly managing connection lifecycles, particularly controlling the maximum number of requests per persistent connection. This mirrors real-world client behavior and can reveal server-side issues related to connection handling.

### Connection Lifecycle Configuration

```rust
/// Connection lifecycle configuration
#[derive(Debug, Clone)]
pub struct ConnectionLifecycleConfig {
    /// Maximum requests per connection before recycling
    pub max_requests_per_connection: MaxRequestsSpec,
    /// Whether to honor keep-alive timeouts from server
    pub honor_keep_alive_timeout: bool,
    /// Maximum connection lifetime regardless of request count
    pub max_connection_lifetime_ms: Option<u64>,
    /// Whether to reuse client IP when recycling connections
    pub preserve_client_ip_on_recycle: bool,
}

/// Maximum requests per connection specification
#[derive(Debug, Clone)]
pub enum MaxRequestsSpec {
    /// Unlimited requests (connection kept until closed by server)
    Unlimited,
    /// Fixed number of requests
    Fixed(u32),
    /// Random number of requests within range (inclusive)
    Range(u32, u32),
}
```

### Connection Tracking and Recycling

The connection pool tracks request counts per connection and manages recycling:

```rust
/// Pooled connection with request limit tracking
pub struct PooledConnection {
    pub id: String,
    pub client_ip: IpAddr,
    pub session_id: Option<String>,
    
    /// Configuration for this connection
    lifecycle_config: ConnectionLifecycleConfig,
    
    /// Number of requests executed on this connection
    requests_executed: AtomicU32,
    
    /// Maximum requests allowed for this connection (if using random range)
    max_requests_limit: u32,
    
    /// Connection creation time
    creation_time: Instant,
    
    // Connection-specific state...
    
    /// Check if connection should be recycled
    fn should_recycle(&self) -> bool {
        // Check request count limit
        match self.lifecycle_config.max_requests_per_connection {
            MaxRequestsSpec::Unlimited => false,
            MaxRequestsSpec::Fixed(limit) => 
                self.requests_executed.load(Ordering::Relaxed) >= limit,
            MaxRequestsSpec::Range(_, _) => 
                self.requests_executed.load(Ordering::Relaxed) >= self.max_requests_limit,
        } || 
        // Check lifetime limit
        self.lifecycle_config.max_connection_lifetime_ms
            .map(|ms| self.creation_time.elapsed() >= Duration::from_millis(ms))
            .unwrap_or(false)
    }
    
    /// Execute request and track count
    async fn execute(&self, request: Request) -> Result<Response, Error> {
        // Check if connection should be recycled first
        if self.should_recycle() {
            return Err(Error::ConnectionRecycled);
        }
        
        // Execute request
        let result = self.client.execute(request).await;
        
        // Increment request count
        self.requests_executed.fetch_add(1, Ordering::Relaxed);
        
        // Return result
        result
    }
}
```

### Connection Pool with Recycling Support

```rust
/// Connection pool with recycling support
pub struct ConnectionPool {
    // Maps connection IDs to connections
    connections: DashMap<String, PooledConnection>,
    // Maps session IDs to connection IDs
    session_connections: DashMap<String, String>,
    // Connection lifecycle configuration
    lifecycle_config: ConnectionLifecycleConfig,
    // RNG for random request limits
    rng: Arc<Mutex<SmallRng>>,
    
    /// Get connection with automatic recycling
    async fn get_connection(&self) -> PooledConnection {
        // Generate a new connection
        self.create_connection().await
    }
    
    /// Get or create a connection for a session with recycling
    async fn get_connection_for_session(&self, session_id: &str) -> PooledConnection {
        // Check if session already has a connection
        if let Some(conn_id) = self.session_connections.get(session_id) {
            if let Some(conn) = self.connections.get(&conn_id) {
                // Check if connection should be recycled
                if !conn.should_recycle() {
                    return conn.clone();
                }
                
                // Connection needs recycling
                self.recycle_connection(conn_id.clone(), Some(session_id)).await
            } else {
                // Connection not found, create new
                self.create_connection_for_session(session_id).await
            }
        } else {
            // Create new connection for this session
            self.create_connection_for_session(session_id).await
        }
    }
    
    /// Recycle a connection but preserve client IP and session association
    async fn recycle_connection(&self, conn_id: String, session_id: Option<&str>) -> PooledConnection {
        // Get old connection
        let old_conn = self.connections.remove(&conn_id).map(|(_, conn)| conn);
        
        // Create new connection, preserving client IP if configured
        let client_ip = if let Some(conn) = &old_conn {
            if self.lifecycle_config.preserve_client_ip_on_recycle {
                Some(conn.client_ip)
            } else {
                None
            }
        } else {
            None
        };
        
        // Create new connection with possibly preserved IP
        let new_conn = self.create_connection_with_ip(client_ip).await;
        
        // Preserve session association if needed
        if let Some(session_id) = session_id {
            self.session_connections.insert(session_id.to_string(), new_conn.id.clone());
        }
        
        new_conn
    }
    
    /// Create a new connection with random request limit if using range
    async fn create_connection_with_ip(&self, client_ip: Option<IpAddr>) -> PooledConnection {
        // Generate max_requests value if using Range
        let max_requests = match self.lifecycle_config.max_requests_per_connection {
            MaxRequestsSpec::Range(min, max) => {
                let mut rng = self.rng.lock().unwrap();
                rng.gen_range(min..=max)
            },
            MaxRequestsSpec::Fixed(limit) => limit,
            MaxRequestsSpec::Unlimited => u32::MAX,
        };
        
        // Create connection with determined limit and IP
        // ...
    }
}
```

## 3.1.7 Executor Architecture

The framework employs a layered approach to request execution, separating connection management policies from the low-level connection handling:

![Executor Architecture](https://mermaid.ink/img/pako:eNp1kk1v2zAMhv8K4XOKxE7qpl1vvWWHAes2YIdehiQyE2HygJG8Liv63yc5TpOkHSFL4vseUiIXUColEXJe6zepmy0T543IQYqMLUToKDzRUuUdxXTFLPnvUKPJiAWvpFIbsISbnNa6HSvXmzOzQdpB-G4kFjwzOGWW0J77S5JnBpv23dTQufZajWl7nZ4f5q0f29f5G17EfnH4EFpyw65Qq1w7GQDFmCylesNRLBQLB-QoMqSTxvYJOcr1IKHVwtBkzWjBNduFsNNxqXapOBzHL5XIxNF6JaSdTcvQgAv2GwR6s9JWmlVcuXe7VBpmakRqizdXY1d3FzrVGNyy_K50ZtuVUY26Qbv_v5LgFm_UqYcRRO_aOWh5GJCGzcnBBc2LRJR-Gm6eotzXTwrYkJq0iXkefzL-XddRI-qmGX5zaY5r81KYpqGu_HRY5c8Uk6-MpWy6p6b_Ap9j1f0?type=png)

### 1. Low-Level ExecutorClient

The ExecutorClient is responsible for the actual connection handling, protocol specifics, and low-level details:

```rust
/// Low-level client handling the actual connections
pub trait ExecutorClient: Send + Sync {
    /// Create a new connection
    async fn connect(&self, target: &Uri) -> Result<Connection, ConnectionError>;
    
    /// Send a request on a given connection
    async fn send_request(
        &self, 
        connection: &mut Connection,
        request: Request<Body>
    ) -> Result<Response<Body>, RequestError>;
    
    /// Check if connection is still usable
    fn is_connection_alive(&self, connection: &Connection) -> bool;
    
    /// Close a connection explicitly
    async fn close_connection(&self, connection: &mut Connection);
    
    /// Get client capabilities
    fn get_capabilities(&self) -> ClientCapabilities;
}

/// Client capabilities description
#[derive(Debug, Clone)]
pub struct ClientCapabilities {
    /// Whether client supports HTTP/2
    pub supports_http2: bool,
    /// Whether client supports connection reuse
    pub supports_connection_reuse: bool,
    /// Whether client supports connection pooling
    pub supports_connection_pooling: bool,
    /// Whether the client supports custom TLS configuration
    pub supports_custom_tls: bool,
    /// Maximum concurrent streams for HTTP/2
    pub max_concurrent_streams: Option<u32>,
}

/// Connection representation
#[derive(Debug)]
pub struct Connection {
    /// Unique identifier
    pub id: String,
    /// Remote target
    pub target: Uri,
    /// Connection protocol
    pub protocol: HttpProtocol,
    /// Creation time
    pub created_at: Instant,
    /// Last activity time
    pub last_activity: Instant,
    /// Number of requests sent on this connection
    pub request_count: u32,
    /// Connection-specific state (depends on implementation)
    pub state: ConnectionState,
}

/// HTTP protocol version
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpProtocol {
    Http1,
    Http2,
}

/// Connection state (opaque to high-level executors)
#[derive(Debug)]
pub enum ConnectionState {
    Hyper(HyperConnectionState),
    // Could have other implementations in the future
    Custom(Box<dyn Any + Send + Sync>),
}
```

### 2. Hyper-Based Implementation

```rust
/// Hyper-based implementation of the ExecutorClient
pub struct HyperExecutorClient {
    /// TLS configuration
    tls_config: TlsConfig,
    /// HTTP/2 configuration
    http2_config: Option<Http2Config>,
    /// Hyper client
    client: Client<HttpsConnector<HttpConnector>>,
    /// Connection tracking
    connections: Arc<DashMap<String, HyperConnectionState>>,
    /// Metrics
    metrics: Arc<ExecutorClientMetrics>,
}

impl HyperExecutorClient {
    /// Create a new Hyper-based executor client
    pub fn new(config: HyperClientConfig) -> Result<Self, Box<dyn Error>> {
        // Create custom connector with TLS settings
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        
        // Configure TLS connector
        let mut tls = TlsConnector::builder()
            .min_protocol_version(Some(config.tls_config.min_version))
            .max_protocol_version(Some(config.tls_config.max_version));
            
        // Add cipher suites if specified
        if let Some(ciphers) = &config.tls_config.cipher_list {
            tls.ciphers(ciphers);
        }
        
        // Configure client authentication if needed
        if let (Some(cert_path), Some(key_path)) = (&config.tls_config.cert_path, &config.tls_config.key_path) {
            let identity = Identity::from_pkcs8(
                &std::fs::read(cert_path)?,
                &std::fs::read(key_path)?,
            )?;
            tls.identity(identity);
        }
        
        // Build HTTPS connector
        let https = HttpsConnector::from((http, tls.build()?));
        
        // Create hyper client builder
        let mut client_builder = Client::builder()
            .pool_idle_timeout(config.tls_config.conn_idle_timeout);
            
        // Configure HTTP/2 if enabled
        if let Some(http2_config) = &config.http2_config {
            client_builder = client_builder
                .http2_only(true)
                .http2_initial_stream_window_size(http2_config.initial_window_size)
                .http2_initial_connection_window_size(http2_config.initial_window_size)
                .http2_max_frame_size(http2_config.max_frame_size)
                .http2_keep_alive_interval(Duration::from_secs(http2_config.keep_alive_interval_secs))
                .http2_keep_alive_timeout(Duration::from_secs(http2_config.keep_alive_timeout_secs))
                .http2_keep_alive_while_idle(http2_config.keep_alive_while_idle);
        }
        
        // Build client
        let client = client_builder.build(https);
        
        // Create metrics
        let metrics = Arc::new(ExecutorClientMetrics::new(
            "hyper-client".to_string(), 
            "HyperExecutorClient".to_string()
        ));
        
        Ok(Self {
            tls_config: config.tls_config,
            http2_config: config.http2_config,
            client,
            connections: Arc::new(DashMap::new()),
            metrics,
        })
    }
}

impl ExecutorClient for HyperExecutorClient {
    async fn connect(&self, target: &Uri) -> Result<Connection, ConnectionError> {
        // Create connection ID
        let conn_id = Uuid::new_v4().to_string();
        
        // Determine protocol - if HTTP/2 is configured, use it
        let protocol = if self.http2_config.is_some() {
            HttpProtocol::Http2
        } else {
            HttpProtocol::Http1
        };
        
        // Create connection state
        let state = HyperConnectionState {
            // Specific Hyper state - references to connection pools, etc.
        };
        
        // Store connection state
        self.connections.insert(conn_id.clone(), state.clone());
        
        // Create connection object
        let connection = Connection {
            id: conn_id,
            target: target.clone(),
            protocol,
            created_at: Instant::now(),
            last_activity: Instant::now(),
            request_count: 0,
            state: ConnectionState::Hyper(state),
        };
        
        // Update metrics
        self.metrics.record_connection_created(protocol);
        
        Ok(connection)
    }
    
    async fn send_request(
        &self, 
        connection: &mut Connection,
        request: Request<Body>
    ) -> Result<Response<Body>, RequestError> {
        // Extract Hyper-specific state
        let hyper_state = match &connection.state {
            ConnectionState::Hyper(state) => state,
            _ => return Err(RequestError::Other("Incompatible connection state".to_string())),
        };
        
        // Update last activity time
        connection.last_activity = Instant::now();
        
        // Execute request
        let start = Instant::now();
        let result = self.client.request(request).await;
        let duration = start.elapsed();
        
        // Update request count
        connection.request_count += 1;
        
        // Update metrics
        if let Ok(response) = &result {
            self.metrics.record_successful_request(
                connection.protocol,
                duration,
                response.status().as_u16()
            );
        } else {
            self.metrics.record_failed_request(
                connection.protocol,
                duration
            );
        }
        
        // Map errors
        result.map_err(|e| {
            if e.is_timeout() {
                RequestError::Timeout(e.to_string())
            } else if e.is_connect() {
                RequestError::Connection(e.to_string())
            } else {
                RequestError::Other(e.to_string())
            }
        })
    }
    
    fn is_connection_alive(&self, connection: &Connection) -> bool {
        // Check if the connection is still considered alive
        // For Hyper, connections may be closed by the pool automatically
        match &connection.state {
            ConnectionState::Hyper(_) => {
                // For HTTP/1, check if it's past keep-alive timeout
                // For HTTP/2, it might still be alive due to PING frames
                if connection.protocol == HttpProtocol::Http1 {
                    // Check keep-alive timeout
                    connection.last_activity.elapsed() < Duration::from_secs(30)
                } else {
                    // HTTP/2 connections are considered alive until explicitly closed
                    true
                }
            },
            _ => false,
        }
    }
    
    async fn close_connection(&self, connection: &mut Connection) {
        // Remove from tracking map
        self.connections.remove(&connection.id);
        
        // Update metrics
        self.metrics.record_connection_closed(connection.protocol);
    }
    
    fn get_capabilities(&self) -> ClientCapabilities {
        ClientCapabilities {
            supports_http2: self.http2_config.is_some(),
            supports_connection_reuse: true,
            supports_connection_pooling: true,
            supports_custom_tls: true,
            max_concurrent_streams: self.http2_config.as_ref().map(|c| c.max_concurrent_streams),
        }
    }
}
```

### 3. Standard Executor (Single Request Per Connection)

```rust
/// Standard executor that creates a new connection for each request
pub struct StandardExecutor {
    /// Executor ID
    id: String,
    
    /// Executor configuration
    config: ExecutorConfig,
    
    /// Low-level client implementation
    client: Arc<dyn ExecutorClient>,
    
    /// Metrics collection
    metrics: Arc<ExecutorMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl StandardExecutor {
    /// Create a new standard executor
    pub fn new(
        id: String, 
        config: ExecutorConfig,
        client: Arc<dyn ExecutorClient>
    ) -> Self {
        // Create metrics
        let metrics = Arc::new(ExecutorMetrics::new(
            id.clone(), "StandardExecutor".to_string()
        ));
        
        Self {
            id,
            config,
            client,
            metrics,
            registry_handle: Mutex::new(None),
        }
    }
}

impl RequestExecutor for StandardExecutor {
    async fn execute(
        &self,
        spec: RequestSpec,
        context: &RequestContext,
    ) -> Result<CompletedRequest, RequestError> {
        // Convert URL spec to URI
        let uri = match &spec.url_spec {
            UrlSpec::Static(url) => {
                url.parse::<Uri>().map_err(|e| RequestError::Other(format!("Invalid URL: {}", e)))?
            },
            UrlSpec::Pattern(_) => {
                return Err(RequestError::Other("Unresolved URL pattern".to_string()));
            }
        };
        
        // Create connection for this request
        let mut connection = self.client.connect(&uri).await
            .map_err(|e| RequestError::Connection(e.to_string()))?;
        
        // Build request
        let request = self.build_request(&spec, &connection)?;
        
        // Execute request with timing
        let start_time = Instant::now();
        let response = self.client.send_request(&mut connection, request).await?;
        let end_time = Instant::now();
        
        // Process response
        let status_code = response.status().as_u16();
        let is_error = !response.status().is_success();
        
        // Extract headers
        let headers = response.headers().iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()))
            .collect();
            
        // Read body if this request is sampled
        let (body, response_size) = if context.is_sampled {
            match hyper::body::to_bytes(response.into_body()).await {
                Ok(bytes) => {
                    let size = bytes.len() as u64;
                    let vec = bytes.to_vec();
                    (Some(vec), size)
                },
                Err(_) => (None, 0),
            }
        } else {
            (None, 0)
        };
        
        // Close connection after use (one connection per request)
        self.client.close_connection(&mut connection).await;
        
        // Create completed request
        let completed = CompletedRequest {
            request_id: context.request_id,
            start_time,
            end_time,
            status_code,
            is_error,
            error: None,
            response_size,
            headers,
            body,
            transaction_analysis: None,
        };
        
        // Record metrics
        self.metrics.record_request(&completed);
        
        Ok(completed)
    }

    async fn shutdown(&self) {
        // Nothing to do for standard executor
    }
    
    fn get_config(&self) -> ExecutorConfig {
        self.config.clone()
    }
    
    fn get_executor_type(&self) -> ExecutorType {
        ExecutorType::Standard
    }
}
```

### 4. Connection Pooled Executor (HTTP/1.1 and HTTP/2)

```rust
/// Connection pooling executor for HTTP requests (supports HTTP/1.1 and HTTP/2)
pub struct ConnectionPoolExecutor {
    /// Executor ID
    id: String,
    
    /// Executor configuration
    config: ExecutorConfig,
    
    /// Low-level client implementation
    client: Arc<dyn ExecutorClient>,
    
    /// Connection pool
    pool: Arc<ConnectionPool>,
    
    /// Metrics collection
    metrics: Arc<ExecutorMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl ConnectionPoolExecutor {
    /// Create a new connection pool executor
    pub fn new(
        id: String, 
        config: ExecutorConfig,
        client: Arc<dyn ExecutorClient>,
        pool_config: ConnectionPoolConfig,
        lifecycle_config: ConnectionLifecycleConfig
    ) -> Self {
        // Validate client capabilities
        if !client.get_capabilities().supports_connection_reuse {
            panic!("Client does not support connection reuse");
        }
        
        // Create metrics
        let metrics = Arc::new(ExecutorMetrics::new(
            id.clone(), "ConnectionPoolExecutor".to_string()
        ));
        
        // Create connection pool
        let pool = Arc::new(ConnectionPool::new(
            format!("{}-pool", id),
            pool_config,
            lifecycle_config,
            metrics.clone(),
        ));
        
        Self {
            id,
            config,
            client,
            pool,
            metrics,
            registry_handle: Mutex::new(None),
        }
    }
}

impl RequestExecutor for ConnectionPoolExecutor {
    async fn execute(
        &self,
        spec: RequestSpec,
        context: &RequestContext,
    ) -> Result<CompletedRequest, RequestError> {
        // Convert URL spec to URI
        let uri = match &spec.url_spec {
            UrlSpec::Static(url) => {
                url.parse::<Uri>().map_err(|e| RequestError::Other(format!("Invalid URL: {}", e)))?
            },
            UrlSpec::Pattern(_) => {
                return Err(RequestError::Other("Unresolved URL pattern".to_string()));
            }
        };
        
        // Get appropriate connection from pool
        let mut pooled_connection = self.get_connection_for_request(&spec, context, &uri).await?;
        
        // Get underlying connection
        let connection = &mut pooled_connection.connection;
        
        // Build request
        let request = self.build_request(&spec, connection)?;
        
        // Execute request with timing
        let start_time = Instant::now();
        let result = self.client.send_request(connection, request).await;
        let end_time = Instant::now();
        
        // Check if we need to recycle this connection
        let mut should_recycle = false;
        if pooled_connection.should_recycle() {
            should_recycle = true;
        }
        
        // Process result
        match result {
            Ok(response) => {
                // Process successful response
                let status_code = response.status().as_u16();
                let is_error = !response.status().is_success();
                
                // Extract headers
                let headers = response.headers().iter()
                    .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()))
                    .collect();
                    
                // Read body if this request is sampled
                let (body, response_size) = if context.is_sampled {
                    match hyper::body::to_bytes(response.into_body()).await {
                        Ok(bytes) => {
                            let size = bytes.len() as u64;
                            let vec = bytes.to_vec();
                            (Some(vec), size)
                        },
                        Err(_) => (None, 0),
                    }
                } else {
                    (None, 0)
                };
                
                // Create completed request
                let completed = CompletedRequest {
                    request_id: context.request_id,
                    start_time,
                    end_time,
                    status_code,
                    is_error,
                    error: None,
                    response_size,
                    headers,
                    body,
                    transaction_analysis: None,
                };
                
                // Record metrics
                self.metrics.record_request(&completed);
                
                // Return connection to pool or recycle it
                if should_recycle {
                    self.pool.recycle_connection(pooled_connection).await;
                } else {
                    self.pool.release_connection(pooled_connection).await;
                }
                
                Ok(completed)
            },
            Err(e) => {
                // Connection error - recycle it
                self.pool.recycle_connection(pooled_connection).await;
                
                Err(e)
            }
        }
    }

    async fn shutdown(&self) {
        // Close all connections in the pool
        self.pool.shutdown().await;
    }
    
    fn get_config(&self) -> ExecutorConfig {
        self.config.clone()
    }
    
    fn get_executor_type(&self) -> ExecutorType {
        if self.client.get_capabilities().supports_http2 {
            ExecutorType::ConnectionPoolHttp2
        } else {
            ExecutorType::ConnectionPool
        }
    }
}
```

### 5. Connection Pool Implementation

```rust
/// Connection pool with protocol support for both HTTP/1.1 and HTTP/2
pub struct ConnectionPool {
    /// Pool ID
    id: String,
    
    /// Pool configuration
    config: ConnectionPoolConfig,
    
    /// Connection lifecycle configuration
    lifecycle_config: ConnectionLifecycleConfig,
    
    /// Active connections by host
    connections_by_host: Arc<DashMap<String, Vec<PooledConnection>>>,
    
    /// Session to connection mapping
    session_connections: Arc<DashMap<String, String>>,
    
    /// RNG for random request limits
    rng: Arc<Mutex<SmallRng>>,
    
    /// Metrics
    metrics: Arc<ExecutorMetrics>,
}

impl ConnectionPool {
    /// Create a new connection pool
    pub fn new(
        id: String,
        config: ConnectionPoolConfig,
        lifecycle_config: ConnectionLifecycleConfig,
        metrics: Arc<ExecutorMetrics>
    ) -> Self {
        Self {
            id,
            config,
            lifecycle_config,
            connections_by_host: Arc::new(DashMap::new()),
            session_connections: Arc::new(DashMap::new()),
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            metrics,
        }
    }
    
    /// Get a connection for a host
    async fn get_connection_for_host(&self, host: &str) -> Result<PooledConnection, ConnectionError> {
        // Check if we have available connections for this host
        if let Some(mut entry) = self.connections_by_host.get_mut(host) {
            // Find a connection that doesn't need recycling
            for i in (0..entry.len()).rev() {
                if !entry[i].should_recycle() {
                    return Ok(entry.remove(i));
                }
            }
            
            // All connections need recycling, recycle the last one
            if !entry.is_empty() {
                let conn = entry.pop().unwrap();
                return Ok(conn);
            }
        }
        
        // No suitable connection found, create a new one
        self.create_connection_for_host(host).await
    }
    
    /// Create a new connection for a host
    async fn create_connection_for_host(&self, host: &str) -> Result<PooledConnection, ConnectionError> {
        // Create connection for this host
        // ... (connect to host using client implementation)
        
        // Determine max requests based on configuration
        let max_requests = match self.lifecycle_config.max_requests_per_connection {
            MaxRequestsSpec::Unlimited => u32::MAX,
            MaxRequestsSpec::Fixed(limit) => limit,
            MaxRequestsSpec::Range(min, max) => {
                let mut rng = self.rng.lock().unwrap();
                rng.gen_range(min..=max)
            }
        };
        
        // Create pooled connection
        Ok(PooledConnection {
            id: Uuid::new_v4().to_string(),
            host: host.to_string(),
            client_ip: self.generate_client_ip(),
            session_id: None,
            connection: Connection { /* ... */ },
            max_requests,
            requests_executed: AtomicU32::new(0),
            creation_time: Instant::now(),
            lifecycle_config: Arc::new(self.lifecycle_config.clone()),
        })
    }
    
    /// Get a connection for a session
    async fn get_connection_for_session(&self, session_id: &str, host: &str) -> Result<PooledConnection, ConnectionError> {
        // Check if session already has a connection
        if let Some(conn_id) = self.session_connections.get(session_id) {
            // Find the connection in all hosts
            for entry in self.connections_by_host.iter() {
                for (i, conn) in entry.value().iter().enumerate() {
                    if conn.id == *conn_id {
                        // Found the connection
                        
                        // Check if it should be recycled
                        if conn.should_recycle() {
                            // Recycle the connection but keep session association
                            return self.recycle_connection_for_session(conn_id.clone(), session_id, host).await;
                        }
                        
                        // Return the connection
                        let mut conns = entry.value().clone();
                        let conn = conns.remove(i);
                        self.connections_by_host.insert(entry.key().clone(), conns);
                        return Ok(conn);
                    }
                }
            }
        }
        
        // No existing connection found, create a new one
        let conn = self.create_connection_for_host(host).await?;
        
        // Associate with session
        self.session_connections.insert(session_id.to_string(), conn.id.clone());
        
        Ok(conn)
    }
    
    /// Recycle a connection but preserve client IP and session association
    async fn recycle_connection_for_session(
        &self, 
        conn_id: String,
        session_id: &str,
        host: &str
    ) -> Result<PooledConnection, ConnectionError> {
        // Look for connection to recycle
        for entry in self.connections_by_host.iter() {
            for (i, conn) in entry.value().iter().enumerate() {
                if conn.id == conn_id {
                    // Found the connection
                    
                    // Remember client IP if configured
                    let client_ip = if self.lifecycle_config.preserve_client_ip_on_recycle {
                        Some(conn.client_ip)
                    } else {
                        None
                    };
                    
                    // Close the connection
                    // ... (close connection using client implementation)
                    
                    // Create a new connection
                    let mut new_conn = self.create_connection_for_host(host).await?;
                    
                    // Preserve client IP if configured
                    if let Some(ip) = client_ip {
                        new_conn.client_ip = ip;
                    }
                    
                    // Update session mapping
                    self.session_connections.insert(session_id.to_string(), new_conn.id.clone());
                    
                    return Ok(new_conn);
                }
            }
        }
        
        // Connection not found, create a new one
        let conn = self.create_connection_for_host(host).await?;
        
        // Associate with session
        self.session_connections.insert(session_id.to_string(), conn.id.clone());
        
        Ok(conn)
    }
    
    /// Release a connection back to the pool
    async fn release_connection(&self, connection: PooledConnection) {
        // Check if connection should be recycled
        if connection.should_recycle() {
            self.recycle_connection(connection).await;
            return;
        }
        
        // Return to pool
        let mut entry = self.connections_by_host.entry(connection.host.clone())
            .or_insert_with(Vec::new);
            
        // Check if we have too many connections for this host
        while entry.len() >= self.config.max_connections_per_host as usize {
            // Remove oldest connection
            if let Some(old_conn) = entry.pop() {
                // Close the connection
                // ... (close connection using client implementation)
            }
        }
        
        // Add back to pool
        entry.push(connection);
    }
    
    /// Recycle a connection
    async fn recycle_connection(&self, connection: PooledConnection) {
        // If there's a session mapped to this connection, update the mapping
        for item in self.session_connections.iter() {
            if *item.value() == connection.id {
                // Remove session mapping
                self.session_connections.remove(item.key());
                break;
            }
        }
        
        // Close the connection
        // ... (close connection using client implementation)
        
        // If it's an HTTP/2 connection, we might want to close all streams but keep the connection
        // For simplicity, we'll just create a new connection when needed
    }
    
    /// Generate a client IP
    fn generate_client_ip(&self) -> IpAddr {
        // Generate a random IP address
        let mut rng = self.rng.lock().unwrap();
        let a = rng.gen_range(1..255);
        let b = rng.gen_range(0..255);
        let c = rng.gen_range(0..255);
        let d = rng.gen_range(1..255);
        
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }
    
    /// Shutdown the pool
    async fn shutdown(&self) {
        // Close all connections
        for mut entry in self.connections_by_host.iter_mut() {
            for conn in entry.value_mut().drain(..) {
                // Close the connection
                // ... (close connection using client implementation)
            }
        }
    }
}

/// Pooled connection with request limit tracking
pub struct PooledConnection {
    /// Connection ID
    pub id: String,
    /// Host this connection is for
    pub host: String,
    /// Client IP assigned to this connection
    pub client_ip: IpAddr,
    /// Session ID if this connection is tied to a session
    pub session_id: Option<String>,
    /// The underlying connection
    pub connection: Connection,
    /// Maximum requests allowed for this connection
    pub max_requests: u32,
    /// Number of requests executed on this connection
    pub requests_executed: AtomicU32,
    /// Connection creation time
    pub creation_time: Instant,
    /// Connection lifecycle configuration
    pub lifecycle_config: Arc<ConnectionLifecycleConfig>,
    
    /// Check if connection should be recycled
    pub fn should_recycle(&self) -> bool {
        // Check request count limit
        let request_count = self.requests_executed.load(Ordering::Relaxed);
        let count_exceeded = request_count >= self.max_requests;
        
        // Check connection lifetime limit if configured
        let lifetime_exceeded = if let Some(max_lifetime_ms) = self.lifecycle_config.max_connection_lifetime_ms {
            self.creation_time.elapsed().as_millis() > max_lifetime_ms as u128
        } else {
            false
        };
        
        // Also check if connection is still alive according to the client
        // This would be implementation-specific
        
        count_exceeded || lifetime_exceeded
    }
    
    /// Increment request counter
    pub fn increment_request_counter(&self) {
        self.requests_executed.fetch_add(1, Ordering::Relaxed);
    }
}
```

## 3.1.8 Executor Type Enumeration

```rust
/// Executor type enumeration
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorType {
    /// Standard executor with one connection per request
    Standard,
    /// Connection pooling executor (HTTP/1.1)
    ConnectionPool,
    /// Connection pooling executor with HTTP/2
    ConnectionPoolHttp2,
    /// Custom executor type
    Custom(String),
}

Each executor has specialized coordination with transformers and generators:

1. **StandardExecutor**:
   - Simplest model where each request has its own connection
   - Can use any client IP strategy without coordination concerns
   - Transformers can freely modify client IP headers

2. **ConnectionPoolExecutor**:
   - Maintains a pool of reusable connections
   - Requires session-based client IP assignment
   - Transformers must respect connection session state

3. **Http2Executor**:
   - Multiplexes multiple requests over shared connections
   - Requires connection-based client IP assignment
   - All requests on the same connection must use the same client IP

4. **HyperExecutor** (Custom Implementation):
   - Direct use of hyper for maximum control
   - Can customize TLS versions, cipher suites, TCP options
   - Implements custom connection management with client IP preservation

## 3.1.7 Client IP Implementation Code Flow

The following sequence demonstrates how client IP headers are coordinated across the pipeline:

```rust
// 1. Generator creates initial request with client IP spec
let spec = RequestSpec {
    // ...
    client_ip: Some(ClientIpSpec::SessionBased),
    metadata: RequestMetadata {
        session_id: Some("session123".to_string()),
        // ...
    },
};

// 2. Transformer processes session-based client IP
let transformed_spec = transformer.transform(spec);
// Now client_ip is ClientIpSpec::Static(specific_ip)
// And session headers are added if needed

// 3. Executor selects session-specific connection
let connection = pool.get_connection_for_session(&transformed_spec.metadata.session_id.unwrap());

// 4. Connection ensures client IP header consistency
let request = connection.build_request(transformed_spec);
// X-Forwarded-For header is set to connection.client_ip

// 5. Executor preserves connection for future requests
pool.release_connection(connection);
// Connection remains associated with session_id
```

## 3.1.8 Custom TLS Configuration with Hyper

Using hyper directly allows precise control over TLS and other low-level settings:

```rust
/// Custom Hyper executor with TLS configuration
pub struct HyperExecutor {
    // ...
    tls_config: TlsConfig,
    
    fn build_client(&self) -> Result<Client<HttpsConnector<HttpConnector>, Body>, Error> {
        // Create custom connector with TLS settings
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        
        // Configure TLS connector
        let mut tls = TlsConnector::builder()
            .min_protocol_version(Some(self.tls_config.min_version))
            .max_protocol_version(Some(self.tls_config.max_version));
            
        // Add cipher suites if specified
        if let Some(ciphers) = &self.tls_config.cipher_list {
            tls.ciphers(ciphers);
        }
        
        // Configure client authentication if needed
        if let (Some(cert_path), Some(key_path)) = (&self.tls_config.cert_path, &self.tls_config.key_path) {
            let identity = Identity::from_pkcs8(
                &std::fs::read(cert_path)?,
                &std::fs::read(key_path)?,
            )?;
            tls.identity(identity);
        }
        
        // Build HTTPS connector
        let https = HttpsConnector::from((http, tls.build()?));
        
        // Create hyper client
        let client = Client::builder()
            .pool_idle_timeout(self.tls_config.conn_idle_timeout)
            .http2_only(self.tls_config.http2_only)
            .build(https);
            
        Ok(client)
    }
}

/// TLS configuration
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Minimum TLS version
    pub min_version: rustls::ProtocolVersion,
    /// Maximum TLS version
    pub max_version: rustls::ProtocolVersion,
    /// Cipher suites list
    pub cipher_list: Option<Vec<String>>,
    /// Client certificate path
    pub cert_path: Option<String>,
    /// Client key path
    pub key_path: Option<String>,
    /// Connection idle timeout
    pub conn_idle_timeout: Option<Duration>,
    /// Whether to use HTTP/2 only
    pub http2_only: bool,
}
```

## 3.1.9 Integration of Connection Lifecycle Management in the Pipeline

The executor architecture with connection lifecycle management integrates with the overall request pipeline:

![Connection Lifecycle Integration](https://mermaid.ink/img/pako:eNqNlMFu2zAMhl-F0DlFUidprOyw6zZg6IDdeh6UWE6EyZKnKGuDIs8-ynGyJtsOPojUD38i-YvIEkqtJELBjnojTdMx8daIAqQo2J0IHYUHWql9TzFdM0v-G9RoC2LBjlTqjljCVSGt5gOHlydwnl6SJGdwZN9ND13o10qL9rVu66a_bV_HC9beqlxeQUtu2OWlKrSTAVCMyVKqV0exVCx-I0eRoaM0duiQo1yPEo5WGJrcM1pyzXYx7HReql0qDtP4mUSmz6GQdCKgRbBXrk8VZ9M0RY6lOgq_y6nwRw6GUSJsWCSC-gn6kyxNoziJl0kcJEESL-Mk3IWWUm93oZvVv0iW62US38VBEEZpSw56-XVPUUiTzKhhZ4wadjsRMfZrYqXyUzVKYw0pKk3abF6fM21zWTRNM8q5Ncd98y6MaTKk1o_Du34x-d-yks34V-szfL7H2BE?type=png)

### 1. Pipeline Component Interaction

```rust
// Generator creates initial request
let request_spec = generator.generate(&request_context);

// Transformer processes request (adds headers, IP info)
let transformed_spec = transformer.transform(request_spec);

// Executor (high-level) determines connection management strategy
let pooled_connection = pool.get_connection_for_request(&transformed_spec, &context);

// ExecutorClient (low-level) handles the actual connection details
if pooled_connection.should_recycle() {
    // Connection has reached its request limit
    pool.recycle_connection(pooled_connection);
    
    // Get a fresh connection
    pooled_connection = pool.get_connection_for_request(&transformed_spec, &context);
}

// Execute request with proper headers
let request = build_request(&transformed_spec, &pooled_connection);
let response = executor_client.send_request(&mut pooled_connection.connection, request).await;

// Update request counter on connection
pooled_connection.increment_request_counter();

// Return or recycle connection based on limits
if pooled_connection.should_recycle() {
    pool.recycle_connection(pooled_connection);
} else {
    pool.release_connection(pooled_connection);
}
```

### 2. Client IP Integration Across Connection Recycling

```rust
// When recycling a connection:
async fn recycle_connection(&self, connection: PooledConnection) -> PooledConnection {
    // Remember client IP if configured to preserve
    let client_ip = if self.lifecycle_config.preserve_client_ip_on_recycle {
        Some(connection.client_ip)
    } else {
        None
    };
    
    // Remember session association
    let session_id = connection.session_id.clone();
    
    // Close old connection
    self.client.close_connection(&mut connection.connection).await;
    
    // Create new connection
    let mut new_connection = self.create_new_connection(&connection.host).await;
    
    // Apply preserved client IP if configured
    if let Some(ip) = client_ip {
        new_connection.client_ip = ip;
    }
    
    // Restore session association
    if let Some(sid) = session_id {
        new_connection.session_id = Some(sid.clone());
        self.session_connections.insert(sid, new_connection.id.clone());
    }
    
    new_connection
}
```

### 3. HTTP/2 Considerations

For HTTP/2 connections, the model is adapted to account for the protocol's multiplexing capabilities:

```rust
// For HTTP/2 connections:
impl ConnectionPoolExecutor {
    async fn get_connection_for_request(&self, spec: &RequestSpec, context: &RequestContext, uri: &Uri) -> Result<PooledConnection, ConnectionError> {
        // Get host from URI
        let host = uri.host().unwrap_or("").to_string();
        
        // Check if client supports HTTP/2
        let http2_enabled = self.client.get_capabilities().supports_http2;
        
        if http2_enabled {
            // For HTTP/2, we can have higher request limits per connection
            // since it supports multiplexing
            
            // Check if we have a session ID
            if let Some(session_id) = &spec.metadata.session_id {
                // For sessions, maintain connection affinity
                return self.pool.get_connection_for_session(session_id, &host).await;
            }
            
            // For non-session requests, just get any connection for this host
            return self.pool.get_connection_for_host(&host).await;
        } else {
            // For HTTP/1.1, normal connection pooling rules apply
            
            // Check for session-based connections
            if let Some(session_id) = &spec.metadata.session_id {
                return self.pool.get_connection_for_session(session_id, &host).await;
            }
            
            // Otherwise, get a connection for this host
            return self.pool.get_connection_for_host(&host).await;
        }
    }
}

// HTTP/2 connection has different recycling logic:
impl HyperExecutorClient {
    async fn close_connection(&self, connection: &mut Connection) {
        // For HTTP/2 connections, closing may involve different logic
        if connection.protocol == HttpProtocol::Http2 {
            // For HTTP/2, we might not fully close the connection
            // Just reset the stream counters and state
            // This depends on the specific HTTP/2 implementation
        } else {
            // For HTTP/1.1, normal close logic applies
        }
    }
}
```

## 3.1.11 Example: Combined Slow Client and Request Limit Simulation

This comprehensive example demonstrates how the network condition simulation and connection lifecycle management work together:

```rust
// Configure network simulation
let network_config = NetworkConditionConfig {
    // 20% of connections will be "slow"
    slow_connection_probability: 0.2,
    // Consistent conditions within a session
    consistent_per_session: true,
    // Bandwidth limitations
    bandwidth_config: Some(BandwidthLimitConfig {
        upload_limit: BandwidthLimit::Range(10 * 1024, 50 * 1024),  // 10-50 KB/s upload
        download_limit: BandwidthLimit::Range(20 * 1024, 100 * 1024), // 20-100 KB/s download
        application_strategy: LimitApplicationStrategy::All,
    }),
    // Latency simulation
    latency_config: Some(LatencySimulationConfig {
        base_latency_ms: LatencySpec::Range(50, 200),       // 50-200ms base latency
        jitter_ms: LatencySpec::Range(10, 50),              // 10-50ms jitter
        application_point: LatencyApplicationPoint::Both,   // Apply to both request and response
        application_strategy: LimitApplicationStrategy::All,
    }),
    // Client processing simulation
    processing_delay_config: Some(ProcessingDelayConfig {
        base_processing_ms: ProcessingDelaySpec::Range(100, 500), // 100-500ms processing time
        scale_with_response_size: true,
        scaling_factor_ms_per_kb: 5.0, // 5ms per KB of response data
    }),
};

// Configure connection lifecycle
let lifecycle_config = ConnectionLifecycleConfig {
    // Random number of requests per connection (3-8)
    max_requests_per_connection: MaxRequestsSpec::Range(3, 8),
    honor_keep_alive_timeout: true,
    max_connection_lifetime_ms: Some(30000), // 30 seconds max lifetime
    preserve_client_ip_on_recycle: true,     // Preserve IP across recycling
};

// Create client and pool configurations
let client_config = HyperClientConfig {
    // TLS configuration
    tls_config: TlsConfig {
        min_version: rustls::ProtocolVersion::TLSv1_2,
        max_version: rustls::ProtocolVersion::TLSv1_3,
        cipher_list: Some(vec!["TLS_AES_128_GCM_SHA256".to_string()]),
        cert_path: None,
        key_path: None,
        conn_idle_timeout: Some(Duration::from_secs(60)),
        http2_only: false,
    },
    // HTTP/2 configuration
    http2_config: Some(Http2Config {
        max_concurrent_streams: 100,
        initial_window_size: 65535,
        max_frame_size: 16384,
        keep_alive_interval_secs: 20,
        keep_alive_timeout_secs: 10,
        keep_alive_while_idle: true,
    }),
};

let pool_config = ConnectionPoolConfig {
    max_connections_per_host: 10,
    connection_ttl_secs: 300,
    idle_timeout_secs: 60,
    connection_reuse: true,
    enable_pipelining: false,
};

// Create the executor client with network simulation
let executor_client = Arc::new(HyperExecutorClient::new(
    client_config,
    Some(network_config),
)?);

// Create the connection pool executor
let executor = ConnectionPoolExecutor::new(
    "pool-executor".to_string(),
    ExecutorConfig { /* ... */ },
    executor_client.clone(),
    pool_config,
    lifecycle_config,
);

// Run test with 10 sessions, each with different characteristics
for session_id in 1..=10 {
    let session_str = format!("session-{}", session_id);
    
    // Each session will have multiple requests
    for request_num in 1..=20 {
        // Create context for this request
        let context = RequestContext {
            request_id: (session_id * 1000) + request_num,
            is_sampled: request_num % 5 == 0, // Sample every 5th request
            client_context: if request_num > 1 {
                // After first request, include connection context
                Some(ClientContext {
                    connection_id: format!("conn-{}-{}", session_id, (request_num - 1) / 5), // Will change when recycled
                    client_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, session_id as u8)),
                    session_id: Some(session_str.clone()),
                    requests_on_connection: (request_num - 1) % 5, // Will reset after recycling
                })
            } else {
                None // First request has no client context yet
            },
            parameters: HashMap::new(),
            network_condition_override: None, // Use connection's default network condition
        };
        
        // Generate request
        let spec = generator.generate(&context);
        
        // Add session ID
        let mut transformed_spec = spec;
        transformed_spec.metadata.session_id = Some(session_str.clone());
        
        // Transform request (add headers, etc.)
        let transformed_spec = transformer.transform(transformed_spec);
        
        // Execute request
        let result = executor.execute(transformed_spec, &context).await;
        
        if let Ok(completed) = result {
            // Get network condition that was used (for logging/analysis)
            let network_condition = if let Some(client_ctx) = &completed.client_context {
                client_ctx.network_condition.as_ref().map(|nc| {
                    if nc.is_slow {
                        format!("SLOW: Upload={:?}KB/s, Download={:?}KB/s, Latency={:?}ms",
                            nc.bandwidth_limit_upload.map(|b| b / 1024),
                            nc.bandwidth_limit_download.map(|b| b / 1024),
                            nc.request_latency_ms.unwrap_or(0) + nc.response_latency_ms.unwrap_or(0))
                    } else {
                        "FAST".to_string()
                    }
                })
            } else {
                None
            };
            
            println!("Session {}, Request {}: Status={}, Size={}KB, Network={:?}",
                    session_id, request_num, completed.status_code,
                    completed.response_size / 1024,
                    network_condition.unwrap_or("Unknown".to_string()));
                    
            // Check if connection was recycled (counters reset)
            if let Some(client_ctx) = &completed.client_context {
                if client_ctx.requests_on_connection == 0 && request_num > 1 {
                    println!("  Connection recycled! New connection ID: {}", client_ctx.connection_id);
                }
            }
        } else {
            println!("Session {}, Request {}: ERROR: {:?}", 
                    session_id, request_num, result.err());
        }
        
        // Simulate thinking time between requests (varies by session)
        // Slower "human" behavior for odd-numbered sessions
        if session_id % 2 == 1 {
            let thinking_time = thread_rng().gen_range(500..2000);
            tokio::time::sleep(Duration::from_millis(thinking_time)).await;
        } else {
            // Faster "bot" behavior for even-numbered sessions
            let thinking_time = thread_rng().gen_range(50..200);
            tokio::time::sleep(Duration::from_millis(thinking_time)).await;
        }
    }
}

// Get network simulation metrics
let simulator_metrics = executor_client.get_network_simulator().get_metrics();
println!("Network Simulation Summary:");
println!("  Total connections: {}", 
        simulator_metrics.metrics.get("condition_count")
            .and_then(|v| match v {
                MetricValue::Counter(c) => Some(*c),
                _ => None,
            }).unwrap_or(0));
println!("  Slow connections: {} ({:.1}%)", 
        simulator_metrics.metrics.get("slow_connection_count")
            .and_then(|v| match v {
                MetricValue::Counter(c) => Some(*c),
                _ => None,
            }).unwrap_or(0),
        simulator_metrics.metrics.get("slow_connection_percentage")
            .and_then(|v| match v {
                MetricValue::Float(f) => Some(*f),
                _ => None,
            }).unwrap_or(0.0));
println!("  Avg request delay: {:.1}ms, Avg response delay: {:.1}ms",
        simulator_metrics.metrics.get("total_request_delay_ms")
            .and_then(|v| match v {
                MetricValue::Counter(c) => Some(*c as f64 / 
                    simulator_metrics.metrics.get("latency_added_count")
                        .and_then(|v| match v {
                            MetricValue::Counter(c) => Some(*c as f64),
                            _ => None,
                        }).unwrap_or(1.0)),
                _ => None,
            }).unwrap_or(0.0),
        simulator_metrics.metrics.get("total_response_delay_ms")
            .and_then(|v| match v {
                MetricValue::Counter(c) => Some(*c as f64 / 
                    simulator_metrics.metrics.get("latency_added_count")
                        .and_then(|v| match v {
                            MetricValue::Counter(c) => Some(*c as f64),
                            _ => None,
                        }).unwrap_or(1.0)),
                _ => None,
            }).unwrap_or(0.0));

// Shut down components
executor.shutdown().await;
```

This example demonstrates:

1. **Comprehensive Configuration**:
   - Configures network simulation with bandwidth limits, latency, and processing delays
   - Sets up connection lifecycle with random request limits
   - Creates HTTP/2-capable executor client with proper TLS configuration

2. **Session-Based Testing**:
   - Creates 10 sessions with 20 requests each
   - Preserves connection state across requests in the same session
   - Simulates different user behaviors (slow humans vs fast bots)

3. **Connection Recycling with Network Conditions**:
   - Connections are recycled after reaching their request limits
   - Network conditions are preserved across connection recycling
   - Client IPs remain consistent for each session

4. **Realistic Network Simulation**:
   - 20% of connections are "slow" with bandwidth limits and latency
   - Latency is applied to both requests and responses
   - Processing delays simulate client-side rendering time

5. **Detailed Metrics and Reporting**:
   - Tracks connection recycling events
   - Monitors network simulation statistics
   - Reports performance characteristics for analysis

The combination of connection lifecycle management and network condition simulation creates a highly realistic load testing environment that can reveal issues that only manifest with diverse client behavior patterns.

## 3.1.10 Component Lifecycle and State Management

Components in the pipeline have different lifecycle requirements and state management needs:

1. **Generator**:
   - Typically stateless or using thread-safe RNGs
   - May need configuration but minimal runtime state

2. **Transformer**:
   - May maintain session state across requests
   - Uses thread-safe data structures for concurrent access

3. **Executor**:
   - Manages connection lifecycle and state
   - Handles complex interactions with HTTP protocol
   - Maintains connection pools and client associations

The framework's architecture allows components to be composed while preserving these different state needs through clear interfaces and context sharing.
