# Section 3.1 - Request Pipeline: Generator

## 1. Introduction and Responsibility

The Request Generator is the first component in the request processing pipeline, responsible for creating initial request specifications based on scheduler tickets and client context. Following the single responsibility principle, the generator's sole focus is to transform abstract tickets into concrete request specifications that can be further processed by downstream components.

### 1.1 Core Responsibility

The Generator has a single, well-defined responsibility:
- **Create request specifications** based on tickets, patterns, and context
- Translate abstract scheduler tickets into concrete HTTP request parameters
- Apply predefined URL patterns, methods, and templates
- Generate appropriate bodies and headers based on request type
- Ensure deterministic output for a given input

### 1.2 Position in Pipeline

```
┌────────────────┐       ┌───────────────────┐       ┌────────────────┐
│ Client Session │       │ Request           │       │ Request        │
│ Manager        │──────>│ Scheduler         │──────>│ Generator      │──────>
└────────────────┘       └───────────────────┘       └────────────────┘
                                                            │
                                                            ▼
                                                     ┌────────────────┐
                                                     │ Request        │
                                                     │ Transformer    │
                                                     └────────────────┘
```

The generator receives scheduler tickets and client context, produces request specifications, and passes them to the transformer for further processing.

## 2. Interfaces and Data Structures

### 2.1 Generator Interface

Using static dispatch for performance, the generator interface is designed as a generic trait:

```rust
/// Interface for generating requests with static dispatch
pub trait RequestGenerator<M>: Send + Sync
where
    M: MetricsProvider,
{
    /// Generate a request specification based on context
    fn generate(&self, context: &RequestContext) -> RequestSpec;

    /// Get generator configuration
    fn get_config(&self) -> &GeneratorConfig;
    
    /// Get generator type
    fn get_generator_type(&self) -> GeneratorType;
    
    /// Get metrics provider
    fn metrics(&self) -> &M;
}

/// Generator type for factory pattern
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorType {
    /// Standard template-based generator
    Standard,
    /// Pattern-based generator
    Pattern,
    /// Scenario-based generator
    Scenario,
    /// Custom generator
    Custom(String),
}
```

### 2.2 Request Specification

The request specification is the output of the generator:

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

/// URL specification
#[derive(Debug, Clone)]
pub enum UrlSpec {
    /// Static URL
    Static(String),
    /// Pattern with variable parts
    Pattern(UrlPattern),
}

/// URL pattern with variable parts
#[derive(Debug, Clone)]
pub struct UrlPattern {
    /// Base URL
    pub base: String,
    /// Path components with variables
    pub path_components: Vec<PathComponent>,
    /// Query parameters with variables
    pub query_params: Vec<(String, QueryParamValue)>,
}

/// Path component in URL
#[derive(Debug, Clone)]
pub enum PathComponent {
    /// Static path component
    Static(String),
    /// Variable path component with name
    Variable(String),
}

/// Query parameter value
#[derive(Debug, Clone)]
pub enum QueryParamValue {
    /// Static value
    Static(String),
    /// Variable value with name
    Variable(String),
    /// Random value within constraints
    Random(ValueConstraint),
}

/// Request body specification
#[derive(Debug, Clone)]
pub enum RequestBody {
    /// Empty body
    Empty,
    /// Text body with content type
    Text(String, String),
    /// JSON body
    Json(serde_json::Value),
    /// Form data (key-value pairs)
    Form(Vec<(String, String)>),
    /// Multipart form data
    Multipart(Vec<FormPart>),
    /// Binary data with content type
    Binary(Vec<u8>, String),
    /// Template that needs to be rendered
    Template(BodyTemplate),
}

/// Form part for multipart requests
#[derive(Debug, Clone)]
pub struct FormPart {
    /// Part name
    pub name: String,
    /// File name (if applicable)
    pub filename: Option<String>,
    /// Content type
    pub content_type: String,
    /// Content
    pub content: Vec<u8>,
}

/// Client IP configuration
#[derive(Debug, Clone)]
pub enum ClientIpSpec {
    /// Single static IP
    Static(IpAddr),
    /// Range of IPs to cycle through
    Range(IpAddr, IpAddr),
    /// Random IPs from CIDR block
    RandomFromCidr(String),
    /// Session-based IP (consistent per connection)
    SessionBased,
}

/// Request metadata for tracking
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

### 2.3 Request Context

The generator receives context information:

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
    /// Client session information
    pub session_info: Option<ClientSessionInfo>,
    /// Dynamic parameters that influence generation
    pub parameters: HashMap<String, String>,
}

/// Client session information
#[derive(Debug, Clone)]
pub struct ClientSessionInfo {
    /// Session ID
    pub session_id: String,
    /// Client type
    pub client_type: ClientType,
    /// Session start time
    pub start_time: Instant,
    /// Number of requests in this session so far
    pub request_count: u64,
    /// Client IP address
    pub client_ip: IpAddr,
    /// Client network condition
    pub network_condition: Option<NetworkCondition>,
}
```

### 2.4 Generator Configuration

```rust
/// Configuration for request generator
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// Base URLs to choose from
    pub base_urls: Vec<String>,
    
    /// URL patterns
    pub url_patterns: Vec<UrlPatternConfig>,
    
    /// Request templates
    pub request_templates: Vec<RequestTemplate>,
    
    /// Headers to include in all requests
    pub common_headers: Vec<(String, String)>,
    
    /// Default HTTP method if not specified
    pub default_method: String,
    
    /// Default content type for requests with bodies
    pub default_content_type: String,
    
    /// Client IP address strategy
    pub client_ip_strategy: ClientIpStrategy,
    
    /// Whether to enable correlation
    pub enable_correlation: bool,
    
    /// Parameter templating mode
    pub parameter_mode: ParameterMode,
}

/// Parameter templating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterMode {
    /// Use placeholders like ${param}
    PlaceholderSyntax,
    /// Use handlebar-style {{param}}
    HandlebarSyntax,
    /// Use simple variable names only
    SimpleVariables,
}

/// URL pattern configuration
#[derive(Debug, Clone)]
pub struct UrlPatternConfig {
    /// Pattern name/identifier
    pub name: String,
    
    /// Pattern weight for random selection
    pub weight: u32,
    
    /// Base URL index
    pub base_url_index: usize,
    
    /// Path template
    pub path_template: String,
    
    /// Query parameters
    pub query_params: Vec<QueryParamConfig>,
    
    /// HTTP method
    pub method: String,
    
    /// Expected content type
    pub expected_content_type: Option<String>,
}

/// Query parameter configuration
#[derive(Debug, Clone)]
pub struct QueryParamConfig {
    /// Parameter name
    pub name: String,
    
    /// Parameter value or template
    pub value: QueryParamValueConfig,
    
    /// Whether parameter is required
    pub required: bool,
}

/// Query parameter value configuration
#[derive(Debug, Clone)]
pub enum QueryParamValueConfig {
    /// Static value
    Static(String),
    
    /// Variable reference
    Variable(String),
    
    /// Random value with constraints
    Random(RandomValueConfig),
}

/// Request template
#[derive(Debug, Clone)]
pub struct RequestTemplate {
    /// Template name/identifier
    pub name: String,
    
    /// Template weight for random selection
    pub weight: u32,
    
    /// HTTP method
    pub method: String,
    
    /// URL pattern name
    pub url_pattern: String,
    
    /// Headers specific to this template
    pub headers: Vec<(String, String)>,
    
    /// Body template
    pub body: Option<BodyTemplateConfig>,
    
    /// Tags to apply
    pub tags: HashMap<String, String>,
}

/// Body template configuration
#[derive(Debug, Clone)]
pub enum BodyTemplateConfig {
    /// Text template
    Text(String, String),
    
    /// JSON template
    Json(String),
    
    /// Form template
    Form(Vec<(String, String)>),
    
    /// File reference
    File(String),
}

/// Client IP strategy
#[derive(Debug, Clone)]
pub enum ClientIpStrategy {
    /// Use real client IP
    UseReal,
    
    /// Use single static IP
    UseStatic(String),
    
    /// Use range of IPs
    UseRange(String, String),
    
    /// Random from CIDR block
    RandomFromCidr(String),
    
    /// Session-based (one IP per session)
    SessionBased,
}
```

## 3. Implementation

### 3.1 Standard Generator Implementation

```rust
/// Standard request generator implementation
pub struct StandardGenerator<M, T, C>
where
    M: MetricsProvider,
    T: TemplateRenderer,
    C: ClientIpAllocator,
{
    /// Generator ID
    id: String,
    
    /// Generator configuration
    config: Arc<GeneratorConfig>,
    
    /// Template renderer
    template_renderer: T,
    
    /// URL pattern selector
    url_pattern_selector: UrlPatternSelector,
    
    /// Request template selector
    request_template_selector: RequestTemplateSelector,
    
    /// Client IP allocator
    client_ip_allocator: C,
    
    /// RNG for random selections
    rng: Arc<Mutex<SmallRng>>,
    
    /// Pre-allocated headers buffer for reuse
    headers_buffer: ThreadLocal<RefCell<Vec<(String, String)>>>,
    
    /// Metrics provider
    metrics: M,
}

// Specific construct methods based on parameter mode
impl<M> StandardGenerator<M, PlaceholderTemplateRenderer, RealClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a generator with placeholder syntax and real IP allocation
    pub fn with_placeholder_and_real_ip(
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> Self {
        let template_renderer = PlaceholderTemplateRenderer::new();
        
        // Create URL pattern selector with weighted distribution
        let url_pattern_selector = UrlPatternSelector::new(
            config.url_patterns.iter()
                .map(|p| (p.name.clone(), p.weight))
                .collect(),
        );
        
        // Create request template selector with weighted distribution
        let request_template_selector = RequestTemplateSelector::new(
            config.request_templates.iter()
                .map(|t| (t.name.clone(), t.weight))
                .collect(),
        );
        
        // Create client IP allocator
        let client_ip_allocator = RealClientIpAllocator::new();
        
        Self {
            id,
            config: Arc::new(config),
            template_renderer,
            url_pattern_selector,
            request_template_selector,
            client_ip_allocator,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
}

// Create method that constructs the best concrete implementation
impl<M> StandardGenerator<M, PlaceholderTemplateRenderer, RealClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a new standard generator with static types based on configuration
    pub fn new(
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> impl RequestGenerator<M> {
        // Select based on parameter mode and IP strategy
        match config.parameter_mode {
            ParameterMode::PlaceholderSyntax => {
                match &config.client_ip_strategy {
                    ClientIpStrategy::UseReal => {
                        StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
                    },
                    ClientIpStrategy::UseStatic(ip) => {
                        if let Ok(addr) = ip.parse::<IpAddr>() {
                            StandardGenerator::<M, PlaceholderTemplateRenderer, StaticClientIpAllocator>::with_placeholder_and_static_ip(id, config, addr, metrics)
                        } else {
                            StandardGenerator::<M, PlaceholderTemplateRenderer, FallbackClientIpAllocator>::with_placeholder_and_fallback_ip(id, config, metrics)
                        }
                    },
                    ClientIpStrategy::SessionBased => {
                        StandardGenerator::<M, PlaceholderTemplateRenderer, SessionBasedClientIpAllocator>::with_placeholder_and_session_ip(id, config, metrics)
                    },
                    // Other combinations...
                    _ => {
                        // Default to placeholder and real IP
                        StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
                    }
                }
            },
            ParameterMode::HandlebarSyntax => {
                // Create with Handlebars renderer and appropriate IP allocator
                StandardGenerator::<M, HandlebarTemplateRenderer, RealClientIpAllocator>::with_handlebars_and_real_ip(id, config, metrics)
            },
            ParameterMode::SimpleVariables => {
                // Create with simple renderer and appropriate IP allocator
                StandardGenerator::<M, SimpleTemplateRenderer, RealClientIpAllocator>::with_simple_and_real_ip(id, config, metrics)
            },
        }
    }
}

// Additional constructor implementations for specific combinations
impl<M> StandardGenerator<M, PlaceholderTemplateRenderer, StaticClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a generator with placeholder syntax and static IP allocation
    pub fn with_placeholder_and_static_ip(
        id: String,
        config: GeneratorConfig,
        ip: IpAddr,
        metrics: M,
    ) -> Self {
        let template_renderer = PlaceholderTemplateRenderer::new();
        
        // Create URL pattern selector with weighted distribution
        let url_pattern_selector = UrlPatternSelector::new(
            config.url_patterns.iter()
                .map(|p| (p.name.clone(), p.weight))
                .collect(),
        );
        
        // Create request template selector with weighted distribution
        let request_template_selector = RequestTemplateSelector::new(
            config.request_templates.iter()
                .map(|t| (t.name.clone(), t.weight))
                .collect(),
        );
        
        // Create client IP allocator
        let client_ip_allocator = StaticClientIpAllocator::new(ip);
        
        Self {
            id,
            config: Arc::new(config),
            template_renderer,
            url_pattern_selector,
            request_template_selector,
            client_ip_allocator,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
}

impl<M> StandardGenerator<M, PlaceholderTemplateRenderer, FallbackClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a generator with placeholder syntax and fallback IP allocation
    pub fn with_placeholder_and_fallback_ip(
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> Self {
        let template_renderer = PlaceholderTemplateRenderer::new();
        
        // Create URL pattern selector with weighted distribution
        let url_pattern_selector = UrlPatternSelector::new(
            config.url_patterns.iter()
                .map(|p| (p.name.clone(), p.weight))
                .collect(),
        );
        
        // Create request template selector with weighted distribution
        let request_template_selector = RequestTemplateSelector::new(
            config.request_templates.iter()
                .map(|t| (t.name.clone(), t.weight))
                .collect(),
        );
        
        // Create client IP allocator
        let client_ip_allocator = FallbackClientIpAllocator::new();
        
        Self {
            id,
            config: Arc::new(config),
            template_renderer,
            url_pattern_selector,
            request_template_selector,
            client_ip_allocator,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
}

impl<M> StandardGenerator<M, PlaceholderTemplateRenderer, SessionBasedClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a generator with placeholder syntax and session-based IP allocation
    pub fn with_placeholder_and_session_ip(
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> Self {
        let template_renderer = PlaceholderTemplateRenderer::new();
        
        // Create URL pattern selector with weighted distribution
        let url_pattern_selector = UrlPatternSelector::new(
            config.url_patterns.iter()
                .map(|p| (p.name.clone(), p.weight))
                .collect(),
        );
        
        // Create request template selector with weighted distribution
        let request_template_selector = RequestTemplateSelector::new(
            config.request_templates.iter()
                .map(|t| (t.name.clone(), t.weight))
                .collect(),
        );
        
        // Create client IP allocator
        let client_ip_allocator = SessionBasedClientIpAllocator::new();
        
        Self {
            id,
            config: Arc::new(config),
            template_renderer,
            url_pattern_selector,
            request_template_selector,
            client_ip_allocator,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
}

// Implementations for HandlebarTemplateRenderer combinations
impl<M> StandardGenerator<M, HandlebarTemplateRenderer, RealClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a generator with handlebars syntax and real IP allocation
    pub fn with_handlebars_and_real_ip(
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> Self {
        let template_renderer = HandlebarTemplateRenderer::new();
        
        // Create URL pattern selector with weighted distribution
        let url_pattern_selector = UrlPatternSelector::new(
            config.url_patterns.iter()
                .map(|p| (p.name.clone(), p.weight))
                .collect(),
        );
        
        // Create request template selector with weighted distribution
        let request_template_selector = RequestTemplateSelector::new(
            config.request_templates.iter()
                .map(|t| (t.name.clone(), t.weight))
                .collect(),
        );
        
        // Create client IP allocator
        let client_ip_allocator = RealClientIpAllocator::new();
        
        Self {
            id,
            config: Arc::new(config),
            template_renderer,
            url_pattern_selector,
            request_template_selector,
            client_ip_allocator,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
}

// Implementations for SimpleTemplateRenderer combinations
impl<M> StandardGenerator<M, SimpleTemplateRenderer, RealClientIpAllocator>
where
    M: MetricsProvider,
{
    /// Create a generator with simple syntax and real IP allocation
    pub fn with_simple_and_real_ip(
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> Self {
        let template_renderer = SimpleTemplateRenderer::new();
        
        // Create URL pattern selector with weighted distribution
        let url_pattern_selector = UrlPatternSelector::new(
            config.url_patterns.iter()
                .map(|p| (p.name.clone(), p.weight))
                .collect(),
        );
        
        // Create request template selector with weighted distribution
        let request_template_selector = RequestTemplateSelector::new(
            config.request_templates.iter()
                .map(|t| (t.name.clone(), t.weight))
                .collect(),
        );
        
        // Create client IP allocator
        let client_ip_allocator = RealClientIpAllocator::new();
        
        Self {
            id,
            config: Arc::new(config),
            template_renderer,
            url_pattern_selector,
            request_template_selector,
            client_ip_allocator,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            headers_buffer: ThreadLocal::new(),
            metrics,
        }
    }
}
    
    /// Find a URL pattern by name
    fn find_url_pattern(&self, name: &str) -> Option<&UrlPatternConfig> {
        self.config.url_patterns.iter()
            .find(|p| p.name == name)
    }
    
    /// Find a request template by name
    fn find_request_template(&self, name: &str) -> Option<&RequestTemplate> {
        self.config.request_templates.iter()
            .find(|t| t.name == name)
    }
    
    /// Select a URL pattern based on weights
    fn select_url_pattern(&self) -> Option<&UrlPatternConfig> {
        let mut rng = self.rng.lock().unwrap();
        let pattern_name = self.url_pattern_selector.select(&mut rng)?;
        self.find_url_pattern(&pattern_name)
    }
    
    /// Select a request template based on weights
    fn select_request_template(&self) -> Option<&RequestTemplate> {
        let mut rng = self.rng.lock().unwrap();
        let template_name = self.request_template_selector.select(&mut rng)?;
        self.find_request_template(&template_name)
    }
    
    /// Generate URL spec from pattern and parameters
    fn generate_url_spec(
        &self,
        pattern: &UrlPatternConfig,
        parameters: &HashMap<String, String>,
    ) -> UrlSpec {
        // Get base URL
        let base_index = pattern.base_url_index.min(self.config.base_urls.len() - 1);
        let base_url = &self.config.base_urls[base_index];
        
        // Parse path template
        let path_components = self.parse_path_template(&pattern.path_template);
        
        // Parse query parameters
        let query_params = pattern.query_params.iter()
            .filter_map(|qp| {
                let value = match &qp.value {
                    QueryParamValueConfig::Static(val) => {
                        QueryParamValue::Static(val.clone())
                    },
                    QueryParamValueConfig::Variable(var_name) => {
                        if let Some(val) = parameters.get(var_name) {
                            QueryParamValue::Static(val.clone())
                        } else if qp.required {
                            // Use empty string for required params
                            QueryParamValue::Static(String::new())
                        } else {
                            return None;
                        }
                    },
                    QueryParamValueConfig::Random(config) => {
                        QueryParamValue::Random(self.random_value_constraint(config))
                    },
                };
                
                Some((qp.name.clone(), value))
            })
            .collect();
        
        // Create URL pattern
        let url_pattern = UrlPattern {
            base: base_url.clone(),
            path_components,
            query_params,
        };
        
        UrlSpec::Pattern(url_pattern)
    }
    
    /// Parse path template into components
    fn parse_path_template(&self, template: &str) -> Vec<PathComponent> {
        // Split path by '/' and process each component
        template.trim_start_matches('/')
            .split('/')
            .map(|component| {
                // Check if component is a variable (e.g., ":id" or "{id}")
                if component.starts_with(':') {
                    PathComponent::Variable(component[1..].to_string())
                } else if component.starts_with('{') && component.ends_with('}') {
                    PathComponent::Variable(component[1..component.len()-1].to_string())
                } else {
                    PathComponent::Static(component.to_string())
                }
            })
            .collect()
    }
    
    /// Create value constraint for random parameters
    fn random_value_constraint(&self, config: &RandomValueConfig) -> ValueConstraint {
        // Implementation based on random value config
        match config {
            RandomValueConfig::Integer { min, max } => {
                ValueConstraint::Integer(*min, *max)
            },
            RandomValueConfig::String { pattern, length } => {
                ValueConstraint::String(pattern.clone(), *length)
            },
            // Other random value types...
            _ => ValueConstraint::Default,
        }
    }
    
    /// Generate request body from template
    fn generate_body(
        &self,
        template: &Option<BodyTemplateConfig>,
        parameters: &HashMap<String, String>,
    ) -> Option<RequestBody> {
        if let Some(body_template) = template {
            match body_template {
                BodyTemplateConfig::Text(template_text, content_type) => {
                    // Render text template
                    let rendered = self.template_renderer.render(template_text, parameters);
                    Some(RequestBody::Text(rendered, content_type.clone()))
                },
                BodyTemplateConfig::Json(template_json) => {
                    // Render JSON template
                    let rendered = self.template_renderer.render(template_json, parameters);
                    
                    // Parse as JSON
                    match serde_json::from_str::<serde_json::Value>(&rendered) {
                        Ok(json) => Some(RequestBody::Json(json)),
                        Err(_) => {
                            // Fallback to text if JSON parsing fails
                            Some(RequestBody::Text(rendered, "application/json".to_string()))
                        }
                    }
                },
                BodyTemplateConfig::Form(form_fields) => {
                    // Render form fields
                    let rendered_fields = form_fields.iter()
                        .map(|(name, value_template)| {
                            let rendered_value = self.template_renderer.render(value_template, parameters);
                            (name.clone(), rendered_value)
                        })
                        .collect();
                    
                    Some(RequestBody::Form(rendered_fields))
                },
                BodyTemplateConfig::File(file_path) => {
                    // File loading would be implementation-specific
                    // For now, just return empty body
                    Some(RequestBody::Empty)
                },
            }
        } else {
            None
        }
    }
    
    /// Generate request headers
    fn generate_headers(
        &self,
        template: Option<&RequestTemplate>,
        parameters: &HashMap<String, String>,
    ) -> Vec<(String, String)> {
        // Get thread-local buffer or create new one
        let headers_buffer = self.headers_buffer.get_or(|| RefCell::new(Vec::with_capacity(16)));
        let mut headers = headers_buffer.borrow_mut();
        headers.clear();
        
        // Add common headers first
        for (name, value_template) in &self.config.common_headers {
            let rendered_value = self.template_renderer.render(value_template, parameters);
            headers.push((name.clone(), rendered_value));
        }
        
        // Add template-specific headers if available
        if let Some(tmpl) = template {
            for (name, value_template) in &tmpl.headers {
                // Check if header already exists
                let existing_idx = headers.iter().position(|(n, _)| n == name);
                
                // Render value
                let rendered_value = self.template_renderer.render(value_template, parameters);
                
                if let Some(idx) = existing_idx {
                    // Replace existing header
                    headers[idx] = (name.clone(), rendered_value);
                } else {
                    // Add new header
                    headers.push((name.clone(), rendered_value));
                }
            }
        }
        
        // Clone the headers for returning (buffer stays with thread)
        headers.clone()
    }
    
    /// Generate client IP specification
    fn generate_client_ip(&self, context: &RequestContext) -> Option<ClientIpSpec> {
        // Use client IP allocator to determine appropriate IP spec
        self.client_ip_allocator.allocate_ip(context)
    }
    
    /// Generate request metadata
    fn generate_metadata(&self, context: &RequestContext, template: Option<&RequestTemplate>) -> RequestMetadata {
        let mut metadata = RequestMetadata {
            request_id: context.request_id.to_string(),
            tags: HashMap::new(),
            group_id: None,
            session_id: context.session_info.as_ref().map(|s| s.session_id.clone()),
            enable_correlation: self.config.enable_correlation,
        };
        
        // Add template-specific tags if available
        if let Some(tmpl) = template {
            metadata.tags.extend(tmpl.tags.clone());
        }
        
        // Add session-specific tags if available
        if let Some(session) = &context.session_info {
            metadata.tags.insert("client_type".to_string(), session.client_type.description());
        }
        
        metadata
    }
}

impl<M, T, C> RequestGenerator<M> for StandardGenerator<M, T, C>
where
    M: MetricsProvider,
    T: TemplateRenderer,
    C: ClientIpAllocator,
{
    fn generate(&self, context: &RequestContext) -> RequestSpec {
        let generation_start = Instant::now();
        
        // Select request template (or use null pattern)
        let template = self.select_request_template();
        
        // Get URL pattern from template or select one
        let url_pattern = if let Some(tmpl) = template {
            self.find_url_pattern(&tmpl.url_pattern)
                .or_else(|| self.select_url_pattern())
        } else {
            self.select_url_pattern()
        };
        
        // Generate URL spec
        let url_spec = if let Some(pattern) = url_pattern {
            self.generate_url_spec(pattern, &context.parameters)
        } else {
            // Fallback to static URL if no pattern available
            let default_url = self.config.base_urls.first()
                .cloned()
                .unwrap_or_else(|| "http://localhost".to_string());
                
            UrlSpec::Static(default_url)
        };
        
        // Determine HTTP method
        let method = if let Some(tmpl) = template {
            tmpl.method.clone()
        } else if let Some(pattern) = url_pattern {
            pattern.method.clone()
        } else {
            self.config.default_method.clone()
        };
        
        // Generate headers
        let headers = self.generate_headers(template, &context.parameters);
        
        // Generate body
        let body = if let Some(tmpl) = template {
            self.generate_body(&tmpl.body, &context.parameters)
        } else {
            None
        };
        
        // Generate client IP spec
        let client_ip = self.generate_client_ip(context);
        
        // Generate metadata
        let metadata = self.generate_metadata(context, template);
        
        // Create request spec
        let spec = RequestSpec {
            method,
            url_spec,
            headers,
            body,
            client_ip,
            metadata,
        };
        
        // Record metrics
        let generation_time = generation_start.elapsed();
        self.metrics.record_generation_time(generation_time);
        self.metrics.record_generated_request();
        
        spec
    }

    fn get_config(&self) -> &GeneratorConfig {
        &self.config
    }
    
    fn get_generator_type(&self) -> GeneratorType {
        GeneratorType::Standard
    }
    
    fn metrics(&self) -> &M {
        &self.metrics
    }
}

/// Weighted selector for URL patterns
struct UrlPatternSelector {
    /// Pattern names
    patterns: Vec<String>,
    
    /// Cumulative weights
    weights: Vec<u32>,
    
    /// Total weight
    total_weight: u32,
}

impl UrlPatternSelector {
    /// Create a new URL pattern selector
    pub fn new(pattern_weights: Vec<(String, u32)>) -> Self {
        let mut patterns = Vec::with_capacity(pattern_weights.len());
        let mut weights = Vec::with_capacity(pattern_weights.len());
        let mut cumulative = 0;
        
        for (pattern, weight) in pattern_weights {
            cumulative += weight;
            patterns.push(pattern);
            weights.push(cumulative);
        }
        
        Self {
            patterns,
            weights,
            total_weight: cumulative,
        }
    }
    
    /// Select a pattern based on weights
    pub fn select(&self, rng: &mut SmallRng) -> Option<String> {
        if self.patterns.is_empty() {
            return None;
        }
        
        if self.total_weight == 0 {
            // If all weights are zero, select uniformly
            let idx = rng.gen_range(0..self.patterns.len());
            return Some(self.patterns[idx].clone());
        }
        
        // Generate random value and find corresponding pattern
        let r = rng.gen_range(0..self.total_weight);
        
        let idx = match self.weights.binary_search(&r) {
            Ok(i) => i,
            Err(i) => i,
        };
        
        if idx < self.patterns.len() {
            Some(self.patterns[idx].clone())
        } else {
            // Fallback to last pattern
            Some(self.patterns.last().unwrap().clone())
        }
    }
}

/// Weighted selector for request templates
struct RequestTemplateSelector {
    /// Template names
    templates: Vec<String>,
    
    /// Cumulative weights
    weights: Vec<u32>,
    
    /// Total weight
    total_weight: u32,
}

impl RequestTemplateSelector {
    /// Create a new request template selector
    pub fn new(template_weights: Vec<(String, u32)>) -> Self {
        let mut templates = Vec::with_capacity(template_weights.len());
        let mut weights = Vec::with_capacity(template_weights.len());
        let mut cumulative = 0;
        
        for (template, weight) in template_weights {
            cumulative += weight;
            templates.push(template);
            weights.push(cumulative);
        }
        
        Self {
            templates,
            weights,
            total_weight: cumulative,
        }
    }
    
    /// Select a template based on weights
    pub fn select(&self, rng: &mut SmallRng) -> Option<String> {
        if self.templates.is_empty() {
            return None;
        }
        
        if self.total_weight == 0 {
            // If all weights are zero, select uniformly
            let idx = rng.gen_range(0..self.templates.len());
            return Some(self.templates[idx].clone());
        }
        
        // Generate random value and find corresponding template
        let r = rng.gen_range(0..self.total_weight);
        
        let idx = match self.weights.binary_search(&r) {
            Ok(i) => i,
            Err(i) => i,
        };
        
        if idx < self.templates.len() {
            Some(self.templates[idx].clone())
        } else {
            // Fallback to last template
            Some(self.templates.last().unwrap().clone())
        }
    }
}
```

### 3.2 Template Rendering

```rust
/// Interface for template rendering
pub trait TemplateRenderer: Send + Sync {
    /// Render a template with parameters
    fn render(&self, template: &str, parameters: &HashMap<String, String>) -> String;
}

/// Placeholder template renderer (${param})
pub struct PlaceholderTemplateRenderer;

impl PlaceholderTemplateRenderer {
    /// Create a new placeholder template renderer
    pub fn new() -> Self {
        Self
    }
}

impl TemplateRenderer for PlaceholderTemplateRenderer {
    fn render(&self, template: &str, parameters: &HashMap<String, String>) -> String {
        let mut result = template.to_string();
        
        // Replace all ${param} occurrences
        for (key, value) in parameters {
            let placeholder = format!("${{{}}}", key);
            result = result.replace(&placeholder, value);
        }
        
        result
    }
}

/// Handlebars-style template renderer ({{param}})
pub struct HandlebarTemplateRenderer {
    /// Handlebars engine
    handlebars: Handlebars<'static>,
}

impl HandlebarTemplateRenderer {
    /// Create a new handlebars template renderer
    pub fn new() -> Self {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        
        Self {
            handlebars,
        }
    }
}

impl TemplateRenderer for HandlebarTemplateRenderer {
    fn render(&self, template: &str, parameters: &HashMap<String, String>) -> String {
        // Convert parameters to JSON value
        let json_params = serde_json::to_value(parameters).unwrap_or_default();
        
        // Render template
        self.handlebars.render_template(template, &json_params)
            .unwrap_or_else(|_| template.to_string())
    }
}

/// Simple variable-only template renderer
pub struct SimpleTemplateRenderer;

impl SimpleTemplateRenderer {
    /// Create a new simple template renderer
    pub fn new() -> Self {
        Self
    }
}

impl TemplateRenderer for SimpleTemplateRenderer {
    fn render(&self, template: &str, parameters: &HashMap<String, String>) -> String {
        // If template exactly matches a parameter name, substitute it
        if let Some(value) = parameters.get(template) {
            return value.clone();
        }
        
        // Otherwise, return template as-is
        template.to_string()
    }
}
```

### 3.3 Client IP Allocation

```rust
/// Interface for client IP allocation
pub trait ClientIpAllocator: Send + Sync {
    /// Allocate client IP based on context
    fn allocate_ip(&self, context: &RequestContext) -> Option<ClientIpSpec>;
}

/// Real client IP allocator
pub struct RealClientIpAllocator;

impl RealClientIpAllocator {
    /// Create a new real client IP allocator
    pub fn new() -> Self {
        Self
    }
}

impl ClientIpAllocator for RealClientIpAllocator {
    fn allocate_ip(&self, context: &RequestContext) -> Option<ClientIpSpec> {
        // Use client IP from session if available
        if let Some(session) = &context.session_info {
            Some(ClientIpSpec::Static(session.client_ip))
        } else {
            // No client IP available
            None
        }
    }
}

/// Static client IP allocator
pub struct StaticClientIpAllocator {
    /// Static IP to use
    ip: IpAddr,
}

impl StaticClientIpAllocator {
    /// Create a new static client IP allocator
    pub fn new(ip: IpAddr) -> Self {
        Self { ip }
    }
}

impl ClientIpAllocator for StaticClientIpAllocator {
    fn allocate_ip(&self, _context: &RequestContext) -> Option<ClientIpSpec> {
        Some(ClientIpSpec::Static(self.ip))
    }
}

/// Range client IP allocator
pub struct RangeClientIpAllocator {
    /// Start IP address
    start: IpAddr,
    
    /// End IP address
    end: IpAddr,
}

impl RangeClientIpAllocator {
    /// Create a new range client IP allocator
    pub fn new(start: IpAddr, end: IpAddr) -> Self {
        Self { start, end }
    }
}

impl ClientIpAllocator for RangeClientIpAllocator {
    fn allocate_ip(&self, _context: &RequestContext) -> Option<ClientIpSpec> {
        Some(ClientIpSpec::Range(self.start, self.end))
    }
}

/// CIDR client IP allocator
pub struct CidrClientIpAllocator {
    /// CIDR block
    cidr: String,
}

impl CidrClientIpAllocator {
    /// Create a new CIDR client IP allocator
    pub fn new(cidr: String) -> Self {
        Self { cidr }
    }
}

impl ClientIpAllocator for CidrClientIpAllocator {
    fn allocate_ip(&self, _context: &RequestContext) -> Option<ClientIpSpec> {
        Some(ClientIpSpec::RandomFromCidr(self.cidr.clone()))
    }
}

/// Session-based client IP allocator
pub struct SessionBasedClientIpAllocator;

impl SessionBasedClientIpAllocator {
    /// Create a new session-based client IP allocator
    pub fn new() -> Self {
        Self
    }
}

impl ClientIpAllocator for SessionBasedClientIpAllocator {
    fn allocate_ip(&self, _context: &RequestContext) -> Option<ClientIpSpec> {
        Some(ClientIpSpec::SessionBased)
    }
}

/// Fallback client IP allocator
pub struct FallbackClientIpAllocator;

impl FallbackClientIpAllocator {
    /// Create a new fallback client IP allocator
    pub fn new() -> Self {
        Self
    }
}

impl ClientIpAllocator for FallbackClientIpAllocator {
    fn allocate_ip(&self, _context: &RequestContext) -> Option<ClientIpSpec> {
        // Create a localhost IP
        Some(ClientIpSpec::Static(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))))
    }
}
```

## 4. Metrics Integration

```rust
/// Generator metrics
pub struct GeneratorMetrics {
    /// Component ID
    id: String,
    
    /// Total requests generated
    requests_generated: AtomicU64,
    
    /// Total generation time (ns)
    total_generation_time_ns: AtomicU64,
    
    /// Maximum generation time (ns)
    max_generation_time_ns: AtomicU64,
    
    /// URL patterns used counter
    url_pattern_counts: Arc<DashMap<String, u64>>,
    
    /// Request templates used counter
    template_counts: Arc<DashMap<String, u64>>,
    
    /// Client IP types used
    client_ip_types: Arc<DashMap<String, u64>>,
    
    /// Last update timestamp
    last_update: AtomicU64,
}

impl GeneratorMetrics {
    /// Create a new generator metrics collection
    pub fn new(id: String) -> Self {
        Self {
            id,
            requests_generated: AtomicU64::new(0),
            total_generation_time_ns: AtomicU64::new(0),
            max_generation_time_ns: AtomicU64::new(0),
            url_pattern_counts: Arc::new(DashMap::new()),
            template_counts: Arc::new(DashMap::new()),
            client_ip_types: Arc::new(DashMap::new()),
            last_update: AtomicU64::new(current_timestamp()),
        }
    }
    
    /// Record a generated request
    pub fn record_generated_request(&self) {
        self.requests_generated.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }
    
    /// Record generation time
    pub fn record_generation_time(&self, time: Duration) {
        let time_ns = time.as_nanos() as u64;
        
        // Update total time
        self.total_generation_time_ns.fetch_add(time_ns, Ordering::Relaxed);
        
        // Update max time if larger
        let current_max = self.max_generation_time_ns.load(Ordering::Relaxed);
        if time_ns > current_max {
            self.max_generation_time_ns.store(time_ns, Ordering::Relaxed);
        }
        
        self.update_timestamp();
    }
    
    /// Record URL pattern usage
    pub fn record_url_pattern_used(&self, pattern_name: &str) {
        let mut entry = self.url_pattern_counts.entry(pattern_name.to_string())
            .or_insert(0);
        *entry += 1;
        
        self.update_timestamp();
    }
    
    /// Record template usage
    pub fn record_template_used(&self, template_name: &str) {
        let mut entry = self.template_counts.entry(template_name.to_string())
            .or_insert(0);
        *entry += 1;
        
        self.update_timestamp();
    }
    
    /// Record client IP type
    pub fn record_client_ip_type(&self, ip_type: &str) {
        let mut entry = self.client_ip_types.entry(ip_type.to_string())
            .or_insert(0);
        *entry += 1;
        
        self.update_timestamp();
    }
    
    /// Update the last timestamp
    fn update_timestamp(&self) {
        self.last_update.store(current_timestamp(), Ordering::Relaxed);
    }
    
    /// Get average generation time in nanoseconds
    pub fn get_avg_generation_time_ns(&self) -> f64 {
        let total_time = self.total_generation_time_ns.load(Ordering::Relaxed);
        let count = self.requests_generated.load(Ordering::Relaxed);
        
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
        metrics.insert("requests_generated".to_string(), 
                      MetricValue::Counter(self.requests_generated.load(Ordering::Relaxed)));
                      
        metrics.insert("total_generation_time_ns".to_string(),
                      MetricValue::Counter(self.total_generation_time_ns.load(Ordering::Relaxed)));
                      
        metrics.insert("max_generation_time_ns".to_string(),
                      MetricValue::Counter(self.max_generation_time_ns.load(Ordering::Relaxed)));
                      
        metrics.insert("avg_generation_time_ns".to_string(),
                      MetricValue::Float(self.get_avg_generation_time_ns()));
        
        // Add URL pattern counts
        let pattern_counts = self.url_pattern_counts.iter()
            .map(|entry| format!("{}:{}", entry.key(), entry.value()))
            .collect::<Vec<_>>()
            .join(",");
            
        metrics.insert("url_pattern_counts".to_string(),
                      MetricValue::Text(pattern_counts));
        
        // Add template counts
        let template_counts = self.template_counts.iter()
            .map(|entry| format!("{}:{}", entry.key(), entry.value()))
            .collect::<Vec<_>>()
            .join(",");
            
        metrics.insert("template_counts".to_string(),
                      MetricValue::Text(template_counts));
        
        // Add client IP types
        let ip_types = self.client_ip_types.iter()
            .map(|entry| format!("{}:{}", entry.key(), entry.value()))
            .collect::<Vec<_>>()
            .join(",");
            
        metrics.insert("client_ip_types".to_string(),
                      MetricValue::Text(ip_types));
        
        metrics
    }
}

impl MetricsProvider for GeneratorMetrics {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RequestGenerator".to_string(),
            component_id: self.id.clone(),
            timestamp: current_timestamp(),
            metrics: self.get_snapshot(),
            status: Some(format!(
                "Generated: {}, Avg Time: {:.2}µs",
                self.requests_generated.load(Ordering::Relaxed),
                self.get_avg_generation_time_ns() / 1000.0
            )),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RequestGenerator"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

## 5. Factory and Builder Integration

```rust
/// Factory for creating generators
pub struct GeneratorFactory;

impl GeneratorFactory {
    /// Create a generator based on configuration
    pub fn create<M: MetricsProvider>(
        generator_type: GeneratorType,
        id: String,
        config: GeneratorConfig,
        metrics: M,
    ) -> impl RequestGenerator<M> {
        match generator_type {
            GeneratorType::Standard => {
                // Statically dispatch to the most appropriate implementation based on config
                match config.parameter_mode {
                    ParameterMode::PlaceholderSyntax => {
                        match &config.client_ip_strategy {
                            ClientIpStrategy::UseReal => {
                                StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
                            },
                            ClientIpStrategy::UseStatic(ip) => {
                                if let Ok(addr) = ip.parse::<IpAddr>() {
                                    StandardGenerator::<M, PlaceholderTemplateRenderer, StaticClientIpAllocator>::with_placeholder_and_static_ip(id, config, addr, metrics)
                                } else {
                                    StandardGenerator::<M, PlaceholderTemplateRenderer, FallbackClientIpAllocator>::with_placeholder_and_fallback_ip(id, config, metrics)
                                }
                            },
                            ClientIpStrategy::SessionBased => {
                                StandardGenerator::<M, PlaceholderTemplateRenderer, SessionBasedClientIpAllocator>::with_placeholder_and_session_ip(id, config, metrics)
                            },
                            // Default for other cases
                            _ => StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
                        }
                    },
                    ParameterMode::HandlebarSyntax => {
                        StandardGenerator::<M, HandlebarTemplateRenderer, RealClientIpAllocator>::with_handlebars_and_real_ip(id, config, metrics)
                    },
                    ParameterMode::SimpleVariables => {
                        StandardGenerator::<M, SimpleTemplateRenderer, RealClientIpAllocator>::with_simple_and_real_ip(id, config, metrics)
                    },
                }
            },
            GeneratorType::Pattern => {
                // For now, the Pattern generator type uses the same StandardGenerator with different configuration
                StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
            },
            GeneratorType::Scenario => {
                // For now, the Scenario generator type uses the same StandardGenerator with different configuration
                StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
            },
            GeneratorType::Custom(_) => {
                // For now, custom generators also use StandardGenerator
                StandardGenerator::<M, PlaceholderTemplateRenderer, RealClientIpAllocator>::with_placeholder_and_real_ip(id, config, metrics)
            },
        }
    }
}

impl LoadTestBuilder {
    /// Use a standard generator
    pub fn with_standard_generator(mut self) -> Self {
        self.generator_type = GeneratorType::Standard;
        self
    }
    
    /// Use a pattern-based generator
    pub fn with_pattern_generator(mut self) -> Self {
        self.generator_type = GeneratorType::Pattern;
        self
    }
    
    /// Use a scenario-based generator
    pub fn with_scenario_generator(mut self) -> Self {
        self.generator_type = GeneratorType::Scenario;
        self
    }
    
    /// Configure generator
    pub fn with_generator_config(mut self, config: GeneratorConfig) -> Self {
        self.generator_config = Some(config);
        self
    }
    
    /// Configure base URLs
    pub fn with_base_urls(mut self, urls: Vec<String>) -> Self {
        if let Some(ref mut config) = self.generator_config {
            config.base_urls = urls;
        } else {
            let mut config = GeneratorConfig::default();
            config.base_urls = urls;
            self.generator_config = Some(config);
        }
        self
    }
    
    /// Configure URL patterns
    pub fn with_url_patterns(mut self, patterns: Vec<UrlPatternConfig>) -> Self {
        if let Some(ref mut config) = self.generator_config {
            config.url_patterns = patterns;
        } else {
            let mut config = GeneratorConfig::default();
            config.url_patterns = patterns;
            self.generator_config = Some(config);
        }
        self
    }
    
    /// Configure request templates
    pub fn with_request_templates(mut self, templates: Vec<RequestTemplate>) -> Self {
        if let Some(ref mut config) = self.generator_config {
            config.request_templates = templates;
        } else {
            let mut config = GeneratorConfig::default();
            config.request_templates = templates;
            self.generator_config = Some(config);
        }
        self
    }
    
    /// Set parameter templating mode
    pub fn with_parameter_mode(mut self, mode: ParameterMode) -> Self {
        if let Some(ref mut config) = self.generator_config {
            config.parameter_mode = mode;
        } else {
            let mut config = GeneratorConfig::default();
            config.parameter_mode = mode;
            self.generator_config = Some(config);
        }
        self
    }
    
    /// Set client IP strategy
    pub fn with_client_ip_strategy(mut self, strategy: ClientIpStrategy) -> Self {
        if let Some(ref mut config) = self.generator_config {
            config.client_ip_strategy = strategy;
        } else {
            let mut config = GeneratorConfig::default();
            config.client_ip_strategy = strategy;
            self.generator_config = Some(config);
        }
        self
    }
    
    /// Build the generator component
    fn build_generator<M: MetricsProvider>(&self, metrics: M) -> impl RequestGenerator<M> {
        // Generate a unique ID for this generator
        let id = format!("generator-{}", Uuid::new_v4());
        
        // Get or create generator config
        let config = self.generator_config.clone()
            .unwrap_or_else(|| GeneratorConfig::default());
        
        // Create generator using the factory which handles static dispatch
        GeneratorFactory::create(
            self.generator_type,
            id,
            config,
            metrics,
        )
    }
}
```

## 6. Usage Examples

### 6.1 Basic Usage

```rust
// Create a standard generator
let metrics = GeneratorMetrics::new("example-generator".to_string());
let config = GeneratorConfig {
    base_urls: vec!["https://api.example.com".to_string()],
    url_patterns: vec![
        UrlPatternConfig {
            name: "users".to_string(),
            weight: 10,
            base_url_index: 0,
            path_template: "/users/{id}".to_string(),
            query_params: vec![
                QueryParamConfig {
                    name: "fields".to_string(),
                    value: QueryParamValueConfig::Static("name,email".to_string()),
                    required: false,
                },
            ],
            method: "GET".to_string(),
            expected_content_type: Some("application/json".to_string()),
        },
    ],
    request_templates: vec![
        RequestTemplate {
            name: "get-user".to_string(),
            weight: 10,
            method: "GET".to_string(),
            url_pattern: "users".to_string(),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("User-Agent".to_string(), "LoadTest/1.0".to_string()),
            ],
            body: None,
            tags: HashMap::new(),
        },
    ],
    common_headers: vec![
        ("X-API-Key".to_string(), "${api_key}".to_string()),
    ],
    default_method: "GET".to_string(),
    default_content_type: "application/json".to_string(),
    client_ip_strategy: ClientIpStrategy::SessionBased,
    enable_correlation: true,
    parameter_mode: ParameterMode::PlaceholderSyntax,
};

let generator = StandardGenerator::new(
    "example-generator".to_string(),
    config,
    metrics,
);

// Create request context
let context = RequestContext {
    request_id: 12345,
    ticket: SchedulerTicket {
        id: 12345,
        scheduled_time: Instant::now(),
        is_warmup: false,
        should_sample: false,
        client_session_id: Some("session-123".to_string()),
        dispatch_time: None,
        scheduling_delay_ns: None,
    },
    timestamp: Instant::now(),
    is_sampled: false,
    session_info: Some(ClientSessionInfo {
        session_id: "session-123".to_string(),
        client_type: DefaultClientType::Browser {
            browser_type: BrowserType::Chrome,
            http2_enabled: true,
            connection_limit: 6,
        },
        start_time: Instant::now(),
        request_count: 0,
        client_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
        network_condition: None,
    }),
    parameters: {
        let mut params = HashMap::new();
        params.insert("id".to_string(), "12345".to_string());
        params.insert("api_key".to_string(), "abcdef123456".to_string());
        params
    },
};

// Generate request
let request_spec = generator.generate(&context);
```

### 6.2 Builder Pattern

```rust
// Using builder pattern
let test = LoadTestBuilder::new()
    .with_base_urls(vec![
        "https://api.example.com".to_string(),
        "https://api-backup.example.com".to_string(),
    ])
    .with_url_patterns(vec![
        UrlPatternConfig {
            name: "users".to_string(),
            weight: 10,
            base_url_index: 0,
            path_template: "/users/{id}".to_string(),
            query_params: vec![],
            method: "GET".to_string(),
            expected_content_type: Some("application/json".to_string()),
        },
        UrlPatternConfig {
            name: "create-user".to_string(),
            weight: 5,
            base_url_index: 0,
            path_template: "/users".to_string(),
            query_params: vec![],
            method: "POST".to_string(),
            expected_content_type: Some("application/json".to_string()),
        },
    ])
    .with_request_templates(vec![
        RequestTemplate {
            name: "get-user".to_string(),
            weight: 10,
            method: "GET".to_string(),
            url_pattern: "users".to_string(),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
            ],
            body: None,
            tags: HashMap::new(),
        },
        RequestTemplate {
            name: "create-user".to_string(),
            weight: 5,
            method: "POST".to_string(),
            url_pattern: "create-user".to_string(),
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
            ],
            body: Some(BodyTemplateConfig::Json(r#"{
                "name": "${name}",
                "email": "${email}",
                "age": ${age}
            }"#.to_string())),
            tags: HashMap::new(),
        },
    ])
    .build();
```

## 7. Conclusion

The Request Generator component follows the single responsibility principle by focusing solely on transforming scheduler tickets into request specifications. Key design aspects include:

1. **Static Dispatch**: The generic `RequestGenerator<M>` trait and the concrete `StandardGenerator<M, T, C>` implementation enable full compile-time resolution and zero virtual dispatch overhead in the critical path. By making the implementation generic over the template renderer and client IP allocator types, we eliminate dynamic dispatch entirely.

2. **Pre-allocation**: The implementation uses pre-allocated thread-local buffers for headers to reduce memory allocations during request generation.

3. **Clean Interfaces**: Well-defined input and output types with clear ownership semantics.

4. **Performance Optimizations**:
   - Thread-local buffer reuse
   - Minimized locking with atomic metrics
   - Efficient template rendering
   - Binary search for weighted selection
   - Monomorphization of all components for inlining opportunities

5. **Component Isolation**: The generator has no dependencies on downstream components, maintaining clean separation of concerns.

6. **Composability**: Different implementations can be created for specialized use cases while maintaining the same interface and static dispatch benefits.

7. **Compile-Time Polymorphism**: By using multiple constructor methods for different combinations of template renderers and IP allocators, we ensure the right concrete types are selected at compile time, allowing the compiler to perform maximum optimizations.

By creating a focused, high-performance generator component with zero dynamic dispatch, we've established the foundation for an efficient and scalable request processing pipeline where the most frequently executed code paths benefit from full compiler optimization.
