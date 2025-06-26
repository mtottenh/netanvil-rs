# Section 3.2 - Request Pipeline: Transformer

## 1. Introduction and Responsibility

The Request Transformer is the second component in the request processing pipeline, positioned between the Generator and Executor. Following the single responsibility principle, the transformer's sole responsibility is to modify request specifications before they are passed to the executor.

### 1.1 Core Responsibility

The Transformer has a single, well-defined responsibility:

- **Modify request specifications** based on configured transformation strategies
- Add headers, authentication tokens, and cookies to requests
- Implement protocol-specific transformations (HTTP/1.1, HTTP/2)
- Ensure session correlation and consistency across requests
- Apply client IP mappings and connection management directives

### 1.2 Position in Pipeline

```
┌────────────────┐       ┌───────────────────┐       ┌────────────────┐
│ Request        │       │ Request           │       │ Request        │
│ Generator      │──────>│ Transformer       │──────>│ Executor       │
└────────────────┘       └───────────────────┘       └────────────────┘
                                  │
                                  ▼
                          ┌───────────────────┐
                          │ Session           │
                          │ Correlation Store │
                          └───────────────────┘
```

The transformer receives request specifications from the generator, applies transformations according to configured strategies, and passes the modified specifications to the executor.

## 2. Interfaces and Data Structures

### 2.1 Transformer Interface

Using static dispatch for performance, the transformer interface is designed as a generic trait:

```rust
/// Interface for request transformers with static dispatch
pub trait RequestTransformer<M>: Send + Sync
where
    M: MetricsProvider,
{
    /// Transform a request specification
    fn transform(&self, spec: RequestSpec) -> RequestSpec;

    /// Get transformer configuration
    fn get_config(&self) -> &TransformerConfig;
    
    /// Get transformer type
    fn get_transformer_type(&self) -> TransformerType;
    
    /// Get metrics provider
    fn metrics(&self) -> &M;
}

/// Transformer type for factory pattern
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformerType {
    /// Simple header addition
    Header,
    
    /// Authentication transformer
    Auth,
    
    /// Session correlation transformer
    Session,
    
    /// Composite transformer (applies multiple transformations)
    Composite,
    
    /// Custom transformer
    Custom(String),
}
```

### 2.2 Transformer Configuration

```rust
/// Configuration for request transformer
#[derive(Debug, Clone)]
pub struct TransformerConfig {
    /// Transformation strategies to apply (in order)
    pub strategies: Vec<TransformationStrategy>,
    
    /// HTTP headers to add/modify
    pub headers: Vec<HeaderConfig>,
    
    /// Authentication configuration
    pub auth_config: Option<AuthConfig>,
    
    /// Session correlation configuration
    pub session_config: Option<SessionConfig>,
    
    /// Client IP handling configuration
    pub client_ip_config: Option<ClientIpConfig>,
    
    /// Connection management configuration
    pub connection_config: Option<ConnectionConfig>,
}

/// Transformation strategy
#[derive(Debug, Clone)]
pub enum TransformationStrategy {
    /// Add/modify headers
    Headers,
    
    /// Add authentication
    Authentication,
    
    /// Session correlation
    SessionCorrelation,
    
    /// Client IP handling
    ClientIp,
    
    /// Connection management
    ConnectionManagement,
    
    /// Custom transformation
    Custom(String),
}

/// Header configuration
#[derive(Debug, Clone)]
pub struct HeaderConfig {
    /// Header name
    pub name: String,
    
    /// Header value or template
    pub value: String,
    
    /// When to apply this header
    pub condition: HeaderCondition,
    
    /// Whether to overwrite existing header
    pub overwrite: bool,
}

/// Header application condition
#[derive(Debug, Clone)]
pub enum HeaderCondition {
    /// Always apply
    Always,
    
    /// Apply only to specific URL patterns
    UrlPattern(Vec<String>),
    
    /// Apply only to specific HTTP methods
    Method(Vec<String>),
    
    /// Apply only to requests with specific tags
    Tag(HashMap<String, String>),
    
    /// Apply based on expression
    Expression(String),
}

/// Authentication configuration
#[derive(Debug, Clone)]
pub enum AuthConfig {
    /// Basic authentication
    Basic {
        /// Username
        username: String,
        
        /// Password
        password: String,
    },
    
    /// Bearer token authentication
    Bearer {
        /// Token
        token: String,
        
        /// Token source (static, file, function)
        source: TokenSource,
    },
    
    /// API key authentication
    ApiKey {
        /// Header name
        header: String,
        
        /// Key value
        key: String,
    },
    
    /// Custom authentication
    Custom(String),
}

/// Token source
#[derive(Debug, Clone)]
pub enum TokenSource {
    /// Static token
    Static(String),
    
    /// Load from file
    File(String),
    
    /// Generate from function
    Function(String),
}

/// Session correlation configuration
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Whether to enable session correlation
    pub enabled: bool,
    
    /// Cookie handling strategy
    pub cookie_strategy: CookieStrategy,
    
    /// Header-based session tracking
    pub session_headers: Vec<String>,
    
    /// Correlation header configuration
    pub correlation_header: Option<String>,
}

/// Cookie handling strategy
#[derive(Debug, Clone)]
pub enum CookieStrategy {
    /// Preserve all cookies
    PreserveAll,
    
    /// Preserve specific cookies by name
    PreserveNamed(Vec<String>),
    
    /// Discard all cookies
    DiscardAll,
}

/// Client IP configuration
#[derive(Debug, Clone)]
pub struct ClientIpConfig {
    /// Header to use for client IP
    pub header: String,
    
    /// IP resolution mode
    pub resolution_mode: IpResolutionMode,
    
    /// Whether to preserve IP across connection recycling
    pub preserve_across_recycling: bool,
}

/// IP resolution mode
#[derive(Debug, Clone)]
pub enum IpResolutionMode {
    /// Use header from incoming request
    UseHeader,
    
    /// Resolve from session
    FromSession,
    
    /// Use client IP specification directly
    UseSpec,
}

/// Connection management configuration
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// Headers for connection management
    pub headers: Vec<String>,
    
    /// Connection lifecycle management
    pub lifecycle: ConnectionLifecycle,
}

/// Connection lifecycle
#[derive(Debug, Clone)]
pub enum ConnectionLifecycle {
    /// One request per connection
    OneShot,
    
    /// Keep-alive with max requests
    KeepAlive(u32),
    
    /// Use HTTP/2 multiplexing
    Http2Multiplex,
    
    /// Based on client capabilities
    Adaptive,
}
```

### 2.3 Session Correlation Store

```rust
/// Session store trait with static dispatch
pub trait SessionStore<K, V>: Send + Sync {
    /// Get a value
    fn get(&self, key: &K) -> Option<V>;
    
    /// Set a value
    fn set(&self, key: K, value: V);
    
    /// Remove a value
    fn remove(&self, key: &K) -> Option<V>;
    
    /// Check if a key exists
    fn contains(&self, key: &K) -> bool;
    
    /// Get number of entries
    fn len(&self) -> usize;
    
    /// Check if store is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    
    /// Clear the store
    fn clear(&self);
}

/// Session data for correlation
#[derive(Debug, Clone)]
pub struct SessionData {
    /// Session ID
    pub id: String,
    
    /// Session cookies
    pub cookies: HashMap<String, String>,
    
    /// Session headers
    pub headers: HashMap<String, String>,
    
    /// Captured values from responses
    pub captured_values: HashMap<String, String>,
    
    /// Last active timestamp
    pub last_active: Instant,
    
    /// Session metadata
    pub metadata: HashMap<String, String>,
}

/// Cookie specification
#[derive(Debug, Clone)]
pub struct Cookie {
    /// Cookie name
    pub name: String,
    
    /// Cookie value
    pub value: String,
    
    /// Domain
    pub domain: Option<String>,
    
    /// Path
    pub path: Option<String>,
    
    /// Expires
    pub expires: Option<SystemTime>,
    
    /// Max age in seconds
    pub max_age: Option<u64>,
    
    /// Secure flag
    pub secure: bool,
    
    /// HttpOnly flag
    pub http_only: bool,
    
    /// SameSite policy
    pub same_site: Option<SameSitePolicy>,
}

/// SameSite cookie policy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSitePolicy {
    /// Strict policy
    Strict,
    
    /// Lax policy
    Lax,
    
    /// None policy
    None,
}
```

## 3. Implementation

### 3.1 Transform Strategies with Static Dispatch

Instead of using dynamic dispatch with trait objects, we'll define transform strategies as separate types that implement a common trait. Each transform strategy is statically typed:

```rust
/// Transform strategy trait with static dispatch
pub trait TransformStrategy<S>: Send + Sync 
where
    S: SessionStore<String, SessionData>,
{
    /// Apply transformation to request spec
    fn apply(&self, spec: RequestSpec, config: &TransformerConfig, session_store: &S) -> RequestSpec;
}

/// Header transformation strategy
pub struct HeaderStrategy;

impl<S> TransformStrategy<S> for HeaderStrategy 
where
    S: SessionStore<String, SessionData>,
{
    fn apply(&self, mut spec: RequestSpec, config: &TransformerConfig, _session_store: &S) -> RequestSpec {
        // Get headers to apply
        let headers_to_apply = config.headers.iter()
            .filter(|header_config| {
                match &header_config.condition {
                    HeaderCondition::Always => true,
                    HeaderCondition::UrlPattern(patterns) => {
                        match &spec.url_spec {
                            UrlSpec::Static(url) => {
                                patterns.iter().any(|pattern| url.contains(pattern))
                            },
                            UrlSpec::Pattern(pattern) => {
                                patterns.iter().any(|p| pattern.base.contains(p))
                            },
                        }
                    },
                    HeaderCondition::Method(methods) => {
                        methods.iter().any(|m| m.eq_ignore_ascii_case(&spec.method))
                    },
                    HeaderCondition::Tag(tags) => {
                        tags.iter().all(|(key, value)| {
                            spec.metadata.tags.get(key).map_or(false, |v| v == value)
                        })
                    },
                    HeaderCondition::Expression(_) => {
                        // Expression evaluation would be implementation-specific
                        // For now, always apply
                        true
                    },
                }
            });
        
        // Add or modify headers
        let mut new_headers = spec.headers.clone();
        
        for header_config in headers_to_apply {
            let name = &header_config.name;
            let value = &header_config.value;
            
            if header_config.overwrite {
                // Remove existing header with same name
                new_headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
                
                // Add new header
                new_headers.push((name.clone(), value.clone()));
            } else {
                // Check if header already exists
                let mut found = false;
                
                for (n, v) in &mut new_headers {
                    if n.eq_ignore_ascii_case(name) {
                        // Append to existing header if it's allowed
                        if name.eq_ignore_ascii_case("Cookie") {
                            *v = format!("{}; {}", v, value);
                        } else {
                            *v = value.clone();
                        }
                        found = true;
                        break;
                    }
                }
                
                // Add new header if not found
                if !found {
                    new_headers.push((name.clone(), value.clone()));
                }
            }
        }
        
        spec.headers = new_headers;
        spec
    }
}

/// Authentication transformation strategy
pub struct AuthStrategy;

impl<S> TransformStrategy<S> for AuthStrategy 
where
    S: SessionStore<String, SessionData>,
{
    fn apply(&self, mut spec: RequestSpec, config: &TransformerConfig, _session_store: &S) -> RequestSpec {
        // Apply authentication if configured
        if let Some(auth_config) = &config.auth_config {
            match auth_config {
                AuthConfig::Basic { username, password } => {
                    // Create Basic auth header
                    let auth_value = format!(
                        "Basic {}", 
                        base64::encode(format!("{}:{}", username, password))
                    );
                    
                    // Add or replace Authorization header
                    let mut new_headers = spec.headers.clone();
                    let mut found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case("Authorization") {
                            *value = auth_value.clone();
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push(("Authorization".to_string(), auth_value));
                    }
                    
                    spec.headers = new_headers;
                },
                AuthConfig::Bearer { token, source } => {
                    // Get token based on source
                    let token_value = match source {
                        TokenSource::Static(static_token) => static_token.clone(),
                        TokenSource::File(file_path) => {
                            // In a real implementation, this would read from file
                            // For now, just use the token value directly
                            token.clone()
                        },
                        TokenSource::Function(function_name) => {
                            // In a real implementation, this would call a function
                            // For now, just use the token value directly
                            token.clone()
                        },
                    };
                    
                    // Create Bearer auth header
                    let auth_value = format!("Bearer {}", token_value);
                    
                    // Add or replace Authorization header
                    let mut new_headers = spec.headers.clone();
                    let mut found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case("Authorization") {
                            *value = auth_value.clone();
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push(("Authorization".to_string(), auth_value));
                    }
                    
                    spec.headers = new_headers;
                },
                AuthConfig::ApiKey { header, key } => {
                    // Add or replace API key header
                    let mut new_headers = spec.headers.clone();
                    let mut found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case(header) {
                            *value = key.clone();
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push((header.clone(), key.clone()));
                    }
                    
                    spec.headers = new_headers;
                },
                AuthConfig::Custom(_) => {
                    // Custom authentication would be implementation-specific
                },
            }
        }
        
        spec
    }
}

/// Session correlation transformation strategy
pub struct SessionStrategy;

impl<S> TransformStrategy<S> for SessionStrategy 
where
    S: SessionStore<String, SessionData>,
{
    fn apply(&self, mut spec: RequestSpec, config: &TransformerConfig, session_store: &S) -> RequestSpec {
        // Get session ID from metadata
        let session_id = spec.metadata.session_id.clone();
        
        // Apply session correlation if enabled and session ID exists
        if let (Some(session_config), Some(session_id)) = (&config.session_config, &session_id) {
            if session_config.enabled {
                // Get or create session data
                let session_data = if session_store.contains(&session_id) {
                    session_store.get(&session_id).unwrap_or_else(|| {
                        SessionData {
                            id: session_id.clone(),
                            cookies: HashMap::new(),
                            headers: HashMap::new(),
                            captured_values: HashMap::new(),
                            last_active: Instant::now(),
                            metadata: HashMap::new(),
                        }
                    })
                } else {
                    let data = SessionData {
                        id: session_id.clone(),
                        cookies: HashMap::new(),
                        headers: HashMap::new(),
                        captured_values: HashMap::new(),
                        last_active: Instant::now(),
                        metadata: HashMap::new(),
                    };
                    session_store.set(session_id.clone(), data.clone());
                    data
                };
                
                // Apply cookies from session
                match &session_config.cookie_strategy {
                    CookieStrategy::PreserveAll => {
                        // Add all cookies from session
                        if !session_data.cookies.is_empty() {
                            let cookie_header = session_data.cookies.iter()
                                .map(|(name, value)| format!("{}={}", name, value))
                                .collect::<Vec<_>>()
                                .join("; ");
                            
                            // Find Cookie header or add new one
                            let mut found = false;
                            let mut new_headers = spec.headers.clone();
                            
                            for (name, value) in &mut new_headers {
                                if name.eq_ignore_ascii_case("Cookie") {
                                    *value = if value.is_empty() {
                                        cookie_header.clone()
                                    } else {
                                        format!("{}; {}", value, cookie_header)
                                    };
                                    found = true;
                                    break;
                                }
                            }
                            
                            if !found {
                                new_headers.push(("Cookie".to_string(), cookie_header));
                            }
                            
                            spec.headers = new_headers;
                        }
                    },
                    CookieStrategy::PreserveNamed(names) => {
                        // Add only specified cookies
                        if !session_data.cookies.is_empty() {
                            let cookie_header = session_data.cookies.iter()
                                .filter(|(name, _)| names.contains(name))
                                .map(|(name, value)| format!("{}={}", name, value))
                                .collect::<Vec<_>>()
                                .join("; ");
                            
                            if !cookie_header.is_empty() {
                                // Find Cookie header or add new one
                                let mut found = false;
                                let mut new_headers = spec.headers.clone();
                                
                                for (name, value) in &mut new_headers {
                                    if name.eq_ignore_ascii_case("Cookie") {
                                        *value = if value.is_empty() {
                                            cookie_header.clone()
                                        } else {
                                            format!("{}; {}", value, cookie_header)
                                        };
                                        found = true;
                                        break;
                                    }
                                }
                                
                                if !found {
                                    new_headers.push(("Cookie".to_string(), cookie_header));
                                }
                                
                                spec.headers = new_headers;
                            }
                        }
                    },
                    CookieStrategy::DiscardAll => {
                        // Remove Cookie header if present
                        let new_headers = spec.headers.iter()
                            .filter(|(name, _)| !name.eq_ignore_ascii_case("Cookie"))
                            .cloned()
                            .collect();
                        
                        spec.headers = new_headers;
                    },
                }
                
                // Apply session headers
                for header_name in &session_config.session_headers {
                    if let Some(value) = session_data.headers.get(header_name) {
                        // Check if header already exists
                        let mut found = false;
                        let mut new_headers = spec.headers.clone();
                        
                        for (name, header_value) in &mut new_headers {
                            if name.eq_ignore_ascii_case(header_name) {
                                *header_value = value.clone();
                                found = true;
                                break;
                            }
                        }
                        
                        if !found {
                            new_headers.push((header_name.clone(), value.clone()));
                        }
                        
                        spec.headers = new_headers;
                    }
                }
                
                // Add correlation header if configured
                if let Some(correlation_header) = &session_config.correlation_header {
                    let mut new_headers = spec.headers.clone();
                    let mut found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case(correlation_header) {
                            *value = session_id.clone();
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push((correlation_header.clone(), session_id.clone()));
                    }
                    
                    spec.headers = new_headers;
                }
                
                // Update session data in store with last active time
                let mut updated_session = session_data.clone();
                updated_session.last_active = Instant::now();
                session_store.set(session_id.clone(), updated_session);
            }
        }
        
        spec
    }
}

/// Client IP transformation strategy
pub struct ClientIpStrategy;

impl<S> TransformStrategy<S> for ClientIpStrategy 
where
    S: SessionStore<String, SessionData>,
{
    fn apply(&self, mut spec: RequestSpec, config: &TransformerConfig, _session_store: &S) -> RequestSpec {
        if let Some(client_ip_config) = &config.client_ip_config {
            match &client_ip_config.resolution_mode {
                IpResolutionMode::UseHeader => {
                    // No changes needed - executor will handle this
                },
                IpResolutionMode::FromSession => {
                    // Ensure session-based client IP
                    if spec.client_ip.is_none() {
                        spec.client_ip = Some(ClientIpSpec::SessionBased);
                    }
                },
                IpResolutionMode::UseSpec => {
                    // Use the client IP spec directly, no changes needed
                },
            }
        }
        
        spec
    }
}

/// Connection management transformation strategy
pub struct ConnectionStrategy;

impl<S> TransformStrategy<S> for ConnectionStrategy 
where
    S: SessionStore<String, SessionData>,
{
    fn apply(&self, mut spec: RequestSpec, config: &TransformerConfig, _session_store: &S) -> RequestSpec {
        if let Some(connection_config) = &config.connection_config {
            match &connection_config.lifecycle {
                ConnectionLifecycle::OneShot => {
                    // Add Connection: close header
                    let mut new_headers = spec.headers.clone();
                    let mut found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case("Connection") {
                            *value = "close".to_string();
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push(("Connection".to_string(), "close".to_string()));
                    }
                    
                    spec.headers = new_headers;
                },
                ConnectionLifecycle::KeepAlive(max_requests) => {
                    // Add Connection: keep-alive header
                    let mut new_headers = spec.headers.clone();
                    let mut found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case("Connection") {
                            *value = "keep-alive".to_string();
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push(("Connection".to_string(), "keep-alive".to_string()));
                    }
                    
                    // Add Keep-Alive header with max requests
                    found = false;
                    
                    for (name, value) in &mut new_headers {
                        if name.eq_ignore_ascii_case("Keep-Alive") {
                            *value = format!("max={}", max_requests);
                            found = true;
                            break;
                        }
                    }
                    
                    if !found {
                        new_headers.push(("Keep-Alive".to_string(), format!("max={}", max_requests)));
                    }
                    
                    spec.headers = new_headers;
                },
                ConnectionLifecycle::Http2Multiplex => {
                    // For HTTP/2, we don't need specific headers
                    // The executor will handle connection management
                },
                ConnectionLifecycle::Adaptive => {
                    // Adaptive mode - let the executor decide based on protocol
                },
            }
        }
        
        spec
    }
}
```

### 3.2 Composite Transform Strategy with Static Dispatch

Instead of using a vector of trait objects, we'll implement composite strategy patterns with static types:

```rust
/// Composite strategy combining two strategies with static dispatch
pub struct Composite2<S1, S2, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    strategy1: S1,
    strategy2: S2,
    _marker: PhantomData<Store>,
}

impl<S1, S2, Store> Composite2<S1, S2, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    pub fn new(strategy1: S1, strategy2: S2) -> Self {
        Self {
            strategy1,
            strategy2,
            _marker: PhantomData,
        }
    }
}

impl<S1, S2, Store> TransformStrategy<Store> for Composite2<S1, S2, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    fn apply(&self, spec: RequestSpec, config: &TransformerConfig, session_store: &Store) -> RequestSpec {
        // Apply first strategy, then second
        let intermediate = self.strategy1.apply(spec, config, session_store);
        self.strategy2.apply(intermediate, config, session_store)
    }
}

/// Composite strategy combining three strategies with static dispatch
pub struct Composite3<S1, S2, S3, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    S3: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    strategy1: S1,
    strategy2: S2,
    strategy3: S3,
    _marker: PhantomData<Store>,
}

impl<S1, S2, S3, Store> Composite3<S1, S2, S3, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    S3: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    pub fn new(strategy1: S1, strategy2: S2, strategy3: S3) -> Self {
        Self {
            strategy1,
            strategy2,
            strategy3,
            _marker: PhantomData,
        }
    }
}

impl<S1, S2, S3, Store> TransformStrategy<Store> for Composite3<S1, S2, S3, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    S3: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    fn apply(&self, spec: RequestSpec, config: &TransformerConfig, session_store: &Store) -> RequestSpec {
        // Apply each strategy in sequence
        let intermediate1 = self.strategy1.apply(spec, config, session_store);
        let intermediate2 = self.strategy2.apply(intermediate1, config, session_store);
        self.strategy3.apply(intermediate2, config, session_store)
    }
}

/// Composite strategy combining four strategies with static dispatch
pub struct Composite4<S1, S2, S3, S4, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    S3: TransformStrategy<Store>,
    S4: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    strategy1: S1,
    strategy2: S2,
    strategy3: S3,
    strategy4: S4,
    _marker: PhantomData<Store>,
}

impl<S1, S2, S3, S4, Store> Composite4<S1, S2, S3, S4, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    S3: TransformStrategy<Store>,
    S4: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    pub fn new(strategy1: S1, strategy2: S2, strategy3: S3, strategy4: S4) -> Self {
        Self {
            strategy1,
            strategy2,
            strategy3,
            strategy4,
            _marker: PhantomData,
        }
    }
}

impl<S1, S2, S3, S4, Store> TransformStrategy<Store> for Composite4<S1, S2, S3, S4, Store>
where
    S1: TransformStrategy<Store>,
    S2: TransformStrategy<Store>,
    S3: TransformStrategy<Store>,
    S4: TransformStrategy<Store>,
    Store: SessionStore<String, SessionData>,
{
    fn apply(&self, spec: RequestSpec, config: &TransformerConfig, session_store: &Store) -> RequestSpec {
        // Apply each strategy in sequence
        let intermediate1 = self.strategy1.apply(spec, config, session_store);
        let intermediate2 = self.strategy2.apply(intermediate1, config, session_store);
        let intermediate3 = self.strategy3.apply(intermediate2, config, session_store);
        self.strategy4.apply(intermediate3, config, session_store)
    }
}

/// Complete composite strategy combining all standard strategies
pub struct CompleteComposite<Store>
where
    Store: SessionStore<String, SessionData>,
{
    inner: Composite4<HeaderStrategy, AuthStrategy, SessionStrategy, 
                      Composite2<ClientIpStrategy, ConnectionStrategy, Store>, 
                      Store>,
}

impl<Store> CompleteComposite<Store>
where
    Store: SessionStore<String, SessionData>,
{
    pub fn new() -> Self {
        Self {
            inner: Composite4::new(
                HeaderStrategy,
                AuthStrategy,
                SessionStrategy,
                Composite2::new(ClientIpStrategy, ConnectionStrategy),
            ),
        }
    }
}

impl<Store> TransformStrategy<Store> for CompleteComposite<Store>
where
    Store: SessionStore<String, SessionData>,
{
    fn apply(&self, spec: RequestSpec, config: &TransformerConfig, session_store: &Store) -> RequestSpec {
        self.inner.apply(spec, config, session_store)
    }
}
```

### 3.3 Standard Transformer with Static Dispatch

```rust
/// Standard transformer implementation with static dispatch
pub struct StandardTransformer<M, S, T>
where
    M: MetricsProvider,
    S: SessionStore<String, SessionData>,
    T: TransformStrategy<S>,
{
    /// Transformer ID
    id: String,
    
    /// Transformer configuration
    config: Arc<TransformerConfig>,
    
    /// Session store
    session_store: S,
    
    /// Transform strategy
    transform_strategy: T,
    
    /// Pre-allocated headers buffer for reuse
    headers_buffer: ThreadLocal<RefCell<Vec<(String, String)>>>,
    
    /// Metrics provider
    metrics: M,
}

impl<M, S, T> StandardTransformer<M, S, T>
where
    M: MetricsProvider,
    S: SessionStore<String, SessionData>,
    T: TransformStrategy<S>,
{
    /// Create a new standard transformer
    pub fn new(
        id: String,
        config: TransformerConfig,
        session_store: S,
        transform_strategy: T,
        metrics: M,
    ) -> Self {
        Self {
            id,
            config: Arc::new(config),
            session_store,
            transform_strategy,
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
    
    /// Get or create headers buffer for reuse
    fn get_headers_buffer(&self, spec: &RequestSpec) -> Vec<(String, String)> {
        let headers_buffer = self.headers_buffer.get_or(|| RefCell::new(Vec::with_capacity(16)));
        let mut headers = headers_buffer.borrow_mut();
        headers.clear();
        headers.extend_from_slice(&spec.headers);
        headers.clone()
    }
}

impl<M, S, T> RequestTransformer<M> for StandardTransformer<M, S, T>
where
    M: MetricsProvider,
    S: SessionStore<String, SessionData>,
    T: TransformStrategy<S>,
{
    fn transform(&self, spec: RequestSpec) -> RequestSpec {
        let start = Instant::now();
        
        // Apply transformations using the strategy
        let transformed_spec = self.transform_strategy.apply(
            spec, 
            &self.config, 
            &self.session_store
        );
        
        // Record metrics
        let duration = start.elapsed();
        self.metrics.record_transformation_time(duration);
        self.metrics.record_transformed_request();
        
        transformed_spec
    }
    
    fn get_config(&self) -> &TransformerConfig {
        &self.config
    }
    
    fn get_transformer_type(&self) -> TransformerType {
        TransformerType::Composite
    }
    
    fn metrics(&self) -> &M {
        &self.metrics
    }
}
```

### 3.4 Session Store Implementations

```rust
/// In-memory session store implementation with static dispatch
pub struct InMemorySessionStore<V: Clone> {
    /// Session data
    data: Arc<DashMap<String, V>>,
}

impl<V: Clone> InMemorySessionStore<V> {
    /// Create a new in-memory session store
    pub fn new() -> Self {
        Self {
            data: Arc::new(DashMap::new()),
        }
    }
}

impl<V: Clone> SessionStore<String, V> for InMemorySessionStore<V> {
    fn get(&self, key: &String) -> Option<V> {
        self.data.get(key).map(|v| v.clone())
    }
    
    fn set(&self, key: String, value: V) {
        self.data.insert(key, value);
    }
    
    fn remove(&self, key: &String) -> Option<V> {
        self.data.remove(key).map(|(_, v)| v)
    }
    
    fn contains(&self, key: &String) -> bool {
        self.data.contains_key(key)
    }
    
    fn len(&self) -> usize {
        self.data.len()
    }
    
    fn clear(&self) {
        self.data.clear();
    }
}

/// Time-to-live session store with static dispatch
pub struct TtlSessionStore<V: Clone> {
    /// Session data with last access time
    data: Arc<DashMap<String, (V, Instant)>>,
    
    /// Time-to-live in seconds
    ttl: u64,
    
    /// Cleanup interval in seconds
    cleanup_interval: u64,
    
    /// Last cleanup time
    last_cleanup: Arc<AtomicU64>,
}

impl<V: Clone> TtlSessionStore<V> {
    /// Create a new TTL session store
    pub fn new(ttl: u64, cleanup_interval: u64) -> Self {
        let store = Self {
            data: Arc::new(DashMap::new()),
            ttl,
            cleanup_interval,
            last_cleanup: Arc::new(AtomicU64::new(current_timestamp())),
        };
        
        // Create a clone of data and last_cleanup for the cleanup task
        let data = store.data.clone();
        let ttl = store.ttl;
        let cleanup_interval = store.cleanup_interval;
        let last_cleanup = store.last_cleanup.clone();
        
        // Start cleanup task
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(cleanup_interval));
            
            loop {
                interval.tick().await;
                
                // Check if cleanup is needed
                let now = current_timestamp();
                let last = last_cleanup.load(Ordering::Relaxed);
                
                if now - last >= cleanup_interval {
                    // Perform cleanup
                    let expired_keys = data.iter()
                        .filter_map(|entry| {
                            let (key, (_, last_access)) = entry.pair();
                            if last_access.elapsed().as_secs() > ttl {
                                Some(key.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    
                    // Remove expired entries
                    for key in expired_keys {
                        data.remove(&key);
                    }
                    
                    // Update last cleanup time
                    last_cleanup.store(now, Ordering::Relaxed);
                }
            }
        });
        
        store
    }
}

impl<V: Clone> SessionStore<String, V> for TtlSessionStore<V> {
    fn get(&self, key: &String) -> Option<V> {
        if let Some(mut entry) = self.data.get_mut(key) {
            let (value, last_access) = entry.value_mut();
            
            // Update last access time
            *last_access = Instant::now();
            
            // Return cloned value
            Some(value.clone())
        } else {
            None
        }
    }
    
    fn set(&self, key: String, value: V) {
        self.data.insert(key, (value, Instant::now()));
    }
    
    fn remove(&self, key: &String) -> Option<V> {
        self.data.remove(key).map(|(_, (v, _))| v)
    }
    
    fn contains(&self, key: &String) -> bool {
        self.data.contains_key(key)
    }
    
    fn len(&self) -> usize {
        self.data.len()
    }
    
    fn clear(&self) {
        self.data.clear();
    }
}
```

## 4. Metrics Integration

```rust
/// Transformer metrics
pub struct TransformerMetrics {
    /// Component ID
    id: String,
    
    /// Total requests transformed
    requests_transformed: AtomicU64,
    
    /// Total transformation time (ns)
    total_transformation_time_ns: AtomicU64,
    
    /// Maximum transformation time (ns)
    max_transformation_time_ns: AtomicU64,
    
    /// Headers added counter
    headers_added: AtomicU64,
    
    /// Cookies added counter
    cookies_added: AtomicU64,
    
    /// Authentication headers added
    auth_headers_added: AtomicU64,
    
    /// Session correlations applied
    session_correlations: AtomicU64,
    
    /// Client IP transformations applied
    client_ip_transformations: AtomicU64,
    
    /// Connection management transformations applied
    connection_transformations: AtomicU64,
    
    /// Last update timestamp
    last_update: AtomicU64,
}

impl TransformerMetrics {
    /// Create a new transformer metrics collection
    pub fn new(id: String) -> Self {
        Self {
            id,
            requests_transformed: AtomicU64::new(0),
            total_transformation_time_ns: AtomicU64::new(0),
            max_transformation_time_ns: AtomicU64::new(0),
            headers_added: AtomicU64::new(0),
            cookies_added: AtomicU64::new(0),
            auth_headers_added: AtomicU64::new(0),
            session_correlations: AtomicU64::new(0),
            client_ip_transformations: AtomicU64::new(0),
            connection_transformations: AtomicU64::new(0),
            last_update: AtomicU64::new(current_timestamp()),
        }
    }
    
    /// Record a transformed request
    pub fn record_transformed_request(&self) {
        self.requests_transformed.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record transformation time
    pub fn record_transformation_time(&self, time: Duration) {
        let time_ns = time.as_nanos() as u64;
        
        // Update total time
        self.total_transformation_time_ns.fetch_add(time_ns, Ordering::Relaxed);
        
        // Update max time if larger
        let current_max = self.max_transformation_time_ns.load(Ordering::Relaxed);
        if time_ns > current_max {
            self.max_transformation_time_ns.store(time_ns, Ordering::Relaxed);
        }
        
        self.update_timestamp();
    }
    
    /// Record header addition
    pub fn record_header_added(&self) {
        self.headers_added.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record cookie addition
    pub fn record_cookie_added(&self) {
        self.cookies_added.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record authentication header addition
    pub fn record_auth_header_added(&self) {
        self.auth_headers_added.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record session correlation application
    pub fn record_session_correlation(&self) {
        self.session_correlations.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record client IP transformation
    pub fn record_client_ip_transformation(&self) {
        self.client_ip_transformations.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record connection management transformation
    pub fn record_connection_transformation(&self) {
        self.connection_transformations.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Update the last timestamp
    fn update_timestamp(&self) {
        self.last_update.store(current_timestamp(), Ordering::Relaxed);
    }
    
    /// Get average transformation time in nanoseconds
    pub fn get_avg_transformation_time_ns(&self) -> f64 {
        let total_time = self.total_transformation_time_ns.load(Ordering::Relaxed);
        let count = self.requests_transformed.load(Ordering::Relaxed);
        
        if count == 0 {
            0.0
        } else {
            total_time as f64 / count as f64
        }
    }
    
    /// Get a snapshot of metrics
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
        // Add atomic counter values
        metrics.insert("requests_transformed".to_string(), 
                      MetricValue::Counter(self.requests_transformed.load(Ordering::Relaxed)));
                      
        metrics.insert("total_transformation_time_ns".to_string(),
                      MetricValue::Counter(self.total_transformation_time_ns.load(Ordering::Relaxed)));
                      
        metrics.insert("max_transformation_time_ns".to_string(),
                      MetricValue::Counter(self.max_transformation_time_ns.load(Ordering::Relaxed)));
                      
        metrics.insert("avg_transformation_time_ns".to_string(),
                      MetricValue::Float(self.get_avg_transformation_time_ns()));
                      
        metrics.insert("headers_added".to_string(),
                      MetricValue::Counter(self.headers_added.load(Ordering::Relaxed)));
                      
        metrics.insert("cookies_added".to_string(),
                      MetricValue::Counter(self.cookies_added.load(Ordering::Relaxed)));
                      
        metrics.insert("auth_headers_added".to_string(),
                      MetricValue::Counter(self.auth_headers_added.load(Ordering::Relaxed)));
                      
        metrics.insert("session_correlations".to_string(),
                      MetricValue::Counter(self.session_correlations.load(Ordering::Relaxed)));
                      
        metrics.insert("client_ip_transformations".to_string(),
                      MetricValue::Counter(self.client_ip_transformations.load(Ordering::Relaxed)));
                      
        metrics.insert("connection_transformations".to_string(),
                      MetricValue::Counter(self.connection_transformations.load(Ordering::Relaxed)));
        
        metrics
    }
}

impl MetricsProvider for TransformerMetrics {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RequestTransformer".to_string(),
            component_id: self.id.clone(),
            timestamp: current_timestamp(),
            metrics: self.get_snapshot(),
            status: Some(format!(
                "Transformed: {}, Avg Time: {:.2}µs",
                self.requests_transformed.load(Ordering::Relaxed),
                self.get_avg_transformation_time_ns() / 1000.0
            )),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RequestTransformer"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}

/// Helper function to get current timestamp
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
```

## 5. Factory and Builder Integration

The Factory pattern requires special attention to ensure static dispatch while providing runtime flexibility. We use concrete type creation through generics and traits to achieve this.

```rust
/// Type selection for transformer factory
pub struct TransformerTypes;

impl TransformerTypes {
    /// Create and return the appropriate session store type based on configuration
    pub fn create_session_store(ttl_seconds: Option<u64>) -> impl SessionStore<String, SessionData> {
        match ttl_seconds {
            Some(ttl) => TtlSessionStore::new(ttl, ttl / 10),
            None => InMemorySessionStore::new(),
        }
    }
    
    /// Create and return the appropriate transform strategy type based on strategies list
    pub fn create_transform_strategy<Store>(
        strategies: &[TransformationStrategy]
    ) -> impl TransformStrategy<Store>
    where
        Store: SessionStore<String, SessionData>,
    {
        // Check which strategies are requested
        let use_headers = strategies.contains(&TransformationStrategy::Headers);
        let use_auth = strategies.contains(&TransformationStrategy::Authentication);
        let use_session = strategies.contains(&TransformationStrategy::SessionCorrelation);
        let use_client_ip = strategies.contains(&TransformationStrategy::ClientIp);
        let use_connection = strategies.contains(&TransformationStrategy::ConnectionManagement);
        
        // Create strategy based on requested strategies
        // This pattern match ensures static dispatch by returning a concrete type
        match (use_headers, use_auth, use_session, use_client_ip, use_connection) {
            // All strategies enabled
            (true, true, true, true, true) => {
                CompleteComposite::<Store>::new()
            },
            
            // Only headers
            (true, false, false, false, false) => {
                HeaderStrategy
            },
            
            // Only auth
            (false, true, false, false, false) => {
                AuthStrategy
            },
            
            // Only session
            (false, false, true, false, false) => {
                SessionStrategy
            },
            
            // Headers + Auth
            (true, true, false, false, false) => {
                Composite2::new(HeaderStrategy, AuthStrategy)
            },
            
            // Headers + Session
            (true, false, true, false, false) => {
                Composite2::new(HeaderStrategy, SessionStrategy)
            },
            
            // Auth + Session
            (false, true, true, false, false) => {
                Composite2::new(AuthStrategy, SessionStrategy)
            },
            
            // Headers + Auth + Session
            (true, true, true, false, false) => {
                Composite3::new(HeaderStrategy, AuthStrategy, SessionStrategy)
            },
            
            // Default to complete composite if not matched
            _ => {
                CompleteComposite::<Store>::new()
            },
        }
    }
}

/// Factory for creating transformers with static dispatch
pub struct TransformerFactory;

impl TransformerFactory {
    /// Create a transformer with static dispatch
    pub fn create<M: MetricsProvider>(
        transformer_type: TransformerType,
        id: String,
        config: TransformerConfig,
        metrics: M,
    ) -> impl RequestTransformer<M> {
        match transformer_type {
            TransformerType::Header => {
                // Create a header-only transformer
                let session_store = InMemorySessionStore::<SessionData>::new();
                let transform_strategy = HeaderStrategy;
                
                StandardTransformer::new(
                    id,
                    config,
                    session_store,
                    transform_strategy,
                    metrics,
                )
            },
            
            TransformerType::Auth => {
                // Create an authentication transformer
                let session_store = InMemorySessionStore::<SessionData>::new();
                let transform_strategy = AuthStrategy;
                
                StandardTransformer::new(
                    id,
                    config,
                    session_store,
                    transform_strategy,
                    metrics,
                )
            },
            
            TransformerType::Session => {
                // Create a session correlation transformer
                let session_store = TtlSessionStore::<SessionData>::new(3600, 300); // 1 hour TTL, 5 min cleanup
                let transform_strategy = SessionStrategy;
                
                StandardTransformer::new(
                    id,
                    config,
                    session_store,
                    transform_strategy,
                    metrics,
                )
            },
            
            TransformerType::Composite => {
                // Create a full composite transformer
                let session_store = TtlSessionStore::<SessionData>::new(3600, 300);
                let transform_strategy = CompleteComposite::new();
                
                StandardTransformer::new(
                    id,
                    config,
                    session_store,
                    transform_strategy,
                    metrics,
                )
            },
            
            TransformerType::Custom(_) => {
                // Create a custom transformer based on specific configuration
                let session_store = match config.session_config {
                    Some(_) => TtlSessionStore::<SessionData>::new(3600, 300),
                    None => InMemorySessionStore::<SessionData>::new(),
                };
                
                let transform_strategy = TransformerTypes::create_transform_strategy::<
                    TtlSessionStore<SessionData>
                >(&config.strategies);
                
                StandardTransformer::new(
                    id,
                    config,
                    session_store,
                    transform_strategy,
                    metrics,
                )
            },
        }
    }
}

/// Helper trait for transformer building
pub trait TransformerBuilderExt {
    /// Build a transformer with static dispatch
    fn build_transformer<M: MetricsProvider>(&self, metrics: M) -> impl RequestTransformer<M>;
}

impl LoadTestBuilder {
    /// Use a header transformer
    pub fn with_header_transformer(mut self) -> Self {
        self.transformer_type = TransformerType::Header;
        self
    }
    
    /// Use an auth transformer
    pub fn with_auth_transformer(mut self) -> Self {
        self.transformer_type = TransformerType::Auth;
        self
    }
    
    /// Use a session transformer
    pub fn with_session_transformer(mut self) -> Self {
        self.transformer_type = TransformerType::Session;
        self
    }
    
    /// Use a composite transformer
    pub fn with_composite_transformer(mut self) -> Self {
        self.transformer_type = TransformerType::Composite;
        self
    }
    
    /// Configure transformer
    pub fn with_transformer_config(mut self, config: TransformerConfig) -> Self {
        self.transformer_config = Some(config);
        self
    }
    
    /// Configure common headers
    pub fn with_common_headers(mut self, headers: Vec<HeaderConfig>) -> Self {
        if let Some(ref mut config) = self.transformer_config {
            config.headers = headers;
        } else {
            let mut config = TransformerConfig::default();
            config.headers = headers;
            self.transformer_config = Some(config);
        }
        self
    }
    
    /// Configure authentication
    pub fn with_authentication(mut self, auth_config: AuthConfig) -> Self {
        if let Some(ref mut config) = self.transformer_config {
            config.auth_config = Some(auth_config);
        } else {
            let mut config = TransformerConfig::default();
            config.auth_config = Some(auth_config);
            self.transformer_config = Some(config);
        }
        self
    }
    
    /// Configure session correlation
    pub fn with_session_correlation(mut self, session_config: SessionConfig) -> Self {
        if let Some(ref mut config) = self.transformer_config {
            config.session_config = Some(session_config);
        } else {
            let mut config = TransformerConfig::default();
            config.session_config = Some(session_config);
            self.transformer_config = Some(config);
        }
        self
    }
    
    /// Configure client IP handling
    pub fn with_client_ip_config(mut self, client_ip_config: ClientIpConfig) -> Self {
        if let Some(ref mut config) = self.transformer_config {
            config.client_ip_config = Some(client_ip_config);
        } else {
            let mut config = TransformerConfig::default();
            config.client_ip_config = Some(client_ip_config);
            self.transformer_config = Some(config);
        }
        self
    }
    
    /// Configure connection management
    pub fn with_connection_config(mut self, connection_config: ConnectionConfig) -> Self {
        if let Some(ref mut config) = self.transformer_config {
            config.connection_config = Some(connection_config);
        } else {
            let mut config = TransformerConfig::default();
            config.connection_config = Some(connection_config);
            self.transformer_config = Some(config);
        }
        self
    }
}

impl TransformerBuilderExt for LoadTestBuilder {
    /// Build a transformer with static dispatch
    fn build_transformer<M: MetricsProvider>(&self, metrics: M) -> impl RequestTransformer<M> {
        // Generate a unique ID for this transformer
        let id = format!("transformer-{}", Uuid::new_v4());
        
        // Get or create transformer config
        let config = self.transformer_config.clone()
            .unwrap_or_else(|| TransformerConfig::default());
        
        // Create transformer using factory
        TransformerFactory::create(
            self.transformer_type,
            id,
            config,
            metrics,
        )
    }
}
```

## 6. Usage Examples

### 6.1 Basic Usage with Static Dispatch

```rust
// Create a standard transformer with static dispatch
let metrics = TransformerMetrics::new("example-transformer".to_string());

let config = TransformerConfig {
    strategies: vec![
        TransformationStrategy::Headers,
        TransformationStrategy::Authentication,
        TransformationStrategy::SessionCorrelation,
    ],
    headers: vec![
        HeaderConfig {
            name: "User-Agent".to_string(),
            value: "LoadTest/1.0".to_string(),
            condition: HeaderCondition::Always,
            overwrite: true,
        },
        HeaderConfig {
            name: "Accept".to_string(),
            value: "application/json".to_string(),
            condition: HeaderCondition::Method(vec!["GET".to_string(), "POST".to_string()]),
            overwrite: false,
        },
    ],
    auth_config: Some(AuthConfig::ApiKey {
        header: "X-API-Key".to_string(),
        key: "abcdef123456".to_string(),
    }),
    session_config: Some(SessionConfig {
        enabled: true,
        cookie_strategy: CookieStrategy::PreserveAll,
        session_headers: vec!["X-Session-ID".to_string()],
        correlation_header: Some("X-Correlation-ID".to_string()),
    }),
    client_ip_config: Some(ClientIpConfig {
        header: "X-Forwarded-For".to_string(),
        resolution_mode: IpResolutionMode::FromSession,
        preserve_across_recycling: true,
    }),
    connection_config: Some(ConnectionConfig {
        headers: vec!["Connection".to_string(), "Keep-Alive".to_string()],
        lifecycle: ConnectionLifecycle::KeepAlive(10),
    }),
};

// Create a session store
let session_store = InMemorySessionStore::<SessionData>::new();

// Create a composite transform strategy with static dispatch
let transform_strategy = Composite3::new(
    HeaderStrategy,
    AuthStrategy,
    SessionStrategy,
);

// Create a transformer with static dispatch
let transformer = StandardTransformer::new(
    "example-transformer".to_string(),
    config,
    session_store,
    transform_strategy,
    metrics,
);

// Create request spec to transform
let request_spec = RequestSpec {
    method: "GET".to_string(),
    url_spec: UrlSpec::Static("https://api.example.com/users".to_string()),
    headers: vec![
        ("Accept-Language".to_string(), "en-US".to_string()),
    ],
    body: None,
    client_ip: None,
    metadata: RequestMetadata {
        request_id: "12345".to_string(),
        tags: HashMap::new(),
        group_id: None,
        session_id: Some("session-123".to_string()),
        enable_correlation: true,
    },
};

// Transform request
let transformed_spec = transformer.transform(request_spec);
```

### 6.2 Using the Builder Pattern

```rust
// Using builder pattern with static dispatch
let builder = LoadTestBuilder::new()
    .with_composite_transformer()
    .with_common_headers(vec![
        HeaderConfig {
            name: "User-Agent".to_string(),
            value: "LoadTest/1.0".to_string(),
            condition: HeaderCondition::Always,
            overwrite: true,
        },
        HeaderConfig {
            name: "Accept".to_string(),
            value: "application/json".to_string(),
            condition: HeaderCondition::Method(vec!["GET".to_string(), "POST".to_string()]),
            overwrite: false,
        },
    ])
    .with_authentication(AuthConfig::Bearer {
        token: "your-token-here".to_string(),
        source: TokenSource::Static("your-token-here".to_string()),
    })
    .with_session_correlation(SessionConfig {
        enabled: true,
        cookie_strategy: CookieStrategy::PreserveAll,
        session_headers: vec!["X-Session-ID".to_string()],
        correlation_header: Some("X-Correlation-ID".to_string()),
    })
    .with_client_ip_config(ClientIpConfig {
        header: "X-Forwarded-For".to_string(),
        resolution_mode: IpResolutionMode::FromSession,
        preserve_across_recycling: true,
    })
    .with_connection_config(ConnectionConfig {
        headers: vec!["Connection".to_string(), "Keep-Alive".to_string()],
        lifecycle: ConnectionLifecycle::KeepAlive(10),
    });

// Create metrics
let metrics = TransformerMetrics::new("builder-transformer".to_string());

// Build the transformer with static dispatch
let transformer = builder.build_transformer(metrics);

// Create and transform a request
let request_spec = RequestSpec {
    method: "GET".to_string(),
    url_spec: UrlSpec::Static("https://api.example.com/users".to_string()),
    headers: vec![],
    body: None,
    client_ip: None,
    metadata: RequestMetadata {
        request_id: "12345".to_string(),
        tags: HashMap::new(),
        group_id: None,
        session_id: Some("session-123".to_string()),
        enable_correlation: true,
    },
};

let transformed_spec = transformer.transform(request_spec);
```

### 6.3 Creating a Custom Transformer

```rust
// Creating a custom transformer with specific strategies
let metrics = TransformerMetrics::new("custom-transformer".to_string());

// Create a customized configuration
let config = TransformerConfig {
    strategies: vec![
        TransformationStrategy::Headers,
        TransformationStrategy::ClientIp,
        TransformationStrategy::ConnectionManagement,
    ],
    headers: vec![
        HeaderConfig {
            name: "X-Custom-Header".to_string(),
            value: "CustomValue".to_string(),
            condition: HeaderCondition::Always,
            overwrite: true,
        },
    ],
    auth_config: None,
    session_config: None,
    client_ip_config: Some(ClientIpConfig {
        header: "X-Forwarded-For".to_string(),
        resolution_mode: IpResolutionMode::UseSpec,
        preserve_across_recycling: true,
    }),
    connection_config: Some(ConnectionConfig {
        headers: vec![],
        lifecycle: ConnectionLifecycle::Http2Multiplex,
    }),
};

// Create a session store
let session_store = InMemorySessionStore::<SessionData>::new();

// Create a custom composite transform strategy with static dispatch
let transform_strategy = Composite2::new(
    HeaderStrategy,
    Composite2::new(ClientIpStrategy, ConnectionStrategy),
);

// Create the transformer
let transformer = StandardTransformer::new(
    "custom-transformer".to_string(),
    config,
    session_store,
    transform_strategy,
    metrics,
);

// Create and transform a request
let request_spec = RequestSpec {
    method: "POST".to_string(),
    url_spec: UrlSpec::Static("https://api.example.com/data".to_string()),
    headers: vec![],
    body: Some(RequestBody::Json(serde_json::json!({
        "name": "Test Data",
        "value": 123
    }))),
    client_ip: Some(ClientIpSpec::Static(
        "192.168.1.100".parse().unwrap()
    )),
    metadata: RequestMetadata::default(),
};

let transformed_spec = transformer.transform(request_spec);
```

## 7. Conclusion

The Request Transformer component follows the single responsibility principle by focusing solely on modifying request specifications before they are passed to the executor. Key design aspects include:

1. **Full Static Dispatch**: All components use generics and static typing rather than trait objects, allowing the compiler to perform full monomorphization and optimize the code path. This eliminates dynamic dispatch overhead in the critical request transformation path.

2. **Composable Strategy Pattern**: Transformation logic is implemented as separate strategy types that can be composed at compile time using generic composite strategies. This provides flexibility without runtime polymorphism.

3. **Type-Safe Session Stores**: Session storage is abstracted behind the `SessionStore` trait, with implementation-specific types selected at compile time. This allows various storage backends without runtime polymorphism.

4. **Interface Segregation**: Each strategy handles a specific transformation concern, adhering to the interface segregation principle.

5. **Performance Optimizations**:
   - Thread-local buffer reuse for headers
   - Minimized locking with atomic metrics
   - Zero virtual dispatch overhead
   - Lock-free session store implementations
   - Compile-time strategy composition

6. **Factory Pattern with Static Dispatch**: The factory and builder patterns are implemented using generic functions that return concrete types rather than trait objects, maintaining static dispatch while providing runtime configurability.

7. **Tokio Compatibility**: The implementation is designed to work with tokio's async runtime, particularly in the TTL session store's cleanup task.

This design ensures that the request transformation process is both flexible and high-performance, with all type resolution occurring at compile time for maximum optimization.
