# 1. System Architecture Overview

## 1.1 Project Purpose

**NetHammer** is a modular, statistically robust HTTP load generation framework designed to produce accurate performance measurements for network services. Written in Rust for maximum efficiency and reliability, NetHammer addresses critical shortcomings in existing load testing tools:

1. **Coordinated Omission Prevention**: Properly accounts for "missing" high-latency samples that other tools overlook
2. **Kernel-Level Profiling**: Provides true measurement of server performance by separating client-side overhead
3. **Statistical Validity**: Implements unbiased sampling and proper statistical methodology throughout
4. **Composable Design**: Enables flexible configuration through interchangeable components
5. **Advanced Rate Control**: Offers sophisticated request rate management including PID controllers

The framework is particularly suited for performance engineering teams who need accurate measurements for critical production services, enabling precise capacity planning and performance validation.

## 1.2 Design Principles

The framework is governed by several key principles that inform every aspect of its architecture:

### 1.2.1 Composability

Components are designed with well-defined interfaces that can be combined in different ways to create tests for specific needs. Each component focuses on a single responsibility, allowing for flexible test construction without modifying the core system.

### 1.2.2 Statistical Rigor

All measurement and sampling follows proper statistical methodology to ensure unbiased results. Sampling decisions are made before knowing outcomes, timestamps are collected at precise points, and statistical distributions are properly represented.

### 1.2.3 Measurement Accuracy

The framework distinguishes between client-side overhead and actual server performance, providing a true picture of system behavior under load. Kernel-level profiling isolates network transactions from client-side processing.

### 1.2.4 Scalability

From generating modest loads for functional testing to distributed high-volume stress testing, the architecture scales from a single process to a coordinated fleet of load generators, all sharing a consistent control plane.

### 1.2.5 Adaptability

Components can adjust their behavior based on feedback during test execution, enabling dynamic response to changing conditions such as service degradation or resource constraints.

## 1.3 Core System Components

The load testing framework is composed of specialized components that work together but maintain clear separation of concerns:

### 1.3.1 Rate Controllers

Rate controllers determine and adjust the request rate throughout the test. The architecture separates rate calculation from saturation detection, enabling flexible composition:

- **Static Rate**: Maintains a constant request rate throughout the test
- **Step Function**: Changes rate according to a predetermined schedule 
- **PID Controller**: Dynamically adjusts rate to maintain a target metric (latency, error rate)
- **External Signal Controller**: Adjusts rate based on external feedback signals

Saturation awareness is implemented through a composable feedback mechanism:

1. **Saturation Detectors**: Independent components that measure client resource utilization
2. **Signal Publishers**: Publish saturation metrics through channels or network interfaces
3. **Signal Consumers**: Subscribe to saturation metrics and influence rate controllers

This approach enables both local and distributed saturation-aware control:

- **Local Control**: A local task monitors saturation and adjusts the rate controller's target
- **Distributed Control**: A central coordinator aggregates saturation metrics from multiple clients and distributes rate adjustments

Rate controllers serve as the "brain" of the test, making decisions about load intensity based on system responses and test goals, with saturation awareness implemented as a composable feedback loop.

### 1.3.2 Request Schedulers

Schedulers determine precisely when individual requests should be issued to maintain the target rate established by the rate controller. They implement different timing models:

- **Constant Rate**: Issues requests at fixed intervals
- **Poisson Process**: Models random arrivals with exponentially distributed intervals

Schedulers are tightly coupled with the system's concurrency model, often running on dedicated threads with high-precision timing to prevent coordinated omission. The scheduler is the cornerstone of accurate latency measurement, as it ensures that time-keeping properly accounts for service degradation.

### 1.3.3 Request Generators and Transformers

Request generators create the base request specifications, which transformers then modify before execution:

- **Generators**: Create initial request templates with defined URLs, methods, and bodies
- **URL Pattern Generators**: Create dynamic URL patterns based on parameters or templates
- **Transformers**: Apply modifications to requests such as adding headers, changing parameters, or injecting authentication

This pipeline approach enables progressive request construction and transformation.

### 1.3.4 Request Executors

Executors perform the actual HTTP/HTTPS requests against target systems:

- **HTTP Executors**: Execute standard HTTP/1.1 requests
- **HTTP/2 Executors**: Leverage multiplexing for higher efficiency
- **Connection Pool Managers**: Control connection reuse and lifecycle
- **TLS Configuration**: Manage security settings for encrypted connections

Executors implement backpressure mechanisms and concurrency control to prevent client-side bottlenecks.

### 1.3.5 Transaction Profilers

Profilers collect detailed metrics at both kernel and runtime levels:

- **Kernel-Level Profilers**: Use eBPF to trace system calls and socket operations
- **Runtime Profilers**: Instrument Tokio runtime for async operation insights
- **Saturation Analyzers**: Determine when the client is becoming a bottleneck

Profilers provide critical insight into whether observed latency comes from the server under test or from client-side limitations.

### 1.3.6 Results Collectors

Collectors aggregate metrics from completed requests for analysis:

- **Metric Aggregators**: Combine timing data across requests
- **Histogram Builders**: Construct accurate latency distributions
- **Statistical Analyzers**: Calculate percentiles and statistical measures

Collectors implement lock-free algorithms to minimize overhead during high-throughput tests.

### 1.3.7 Sample Recorders

Recorders capture detailed data for selected requests based on unbiased statistical sampling:

- **Unbiased Samplers**: Select requests for detailed analysis before knowing outcomes
- **Reservoir Samplers**: Maintain representative samples over long tests
- **Context Preservers**: Capture full request/response details for sampled requests

Sampling is performed with statistical validity to ensure unbiased measurement.

### 1.3.8 Result Reporters

Reporters format and present test results during and after execution:

- **Real-Time Reporters**: Display ongoing metrics during test execution
- **Console Reporters**: Format results for terminal output with text-based visualizations
- **Data Exporters**: Output results in machine-readable formats (JSON, CSV) for external analysis
- **Streaming Exporters**: Publish metrics to time-series databases or monitoring systems

Reporters transform raw data into actionable insights about system performance without introducing significant overhead.

### 1.3.9 Distributed Coordinators

Coordinators enable multi-node load generation with centralized control:

- **Load Distributors**: Assign appropriate loads to each worker
- **Synchronizers**: Ensure all nodes start/stop simultaneously
- **Aggregators**: Combine results from multiple workers
- **Saturation Detectors**: Identify and reassign load from overloaded workers

This allows for scaling test capacity beyond what a single machine can generate.

### 1.3.10 Metrics Registry

The metrics subsystem provides comprehensive observability across all components while maintaining the high-performance requirements of load testing:

- **Hybrid Registry Pattern**: A central registry that collects metrics from all components
- **Dual Interface Design**: Separate paths for high-throughput operations and monitoring
- **Component Metrics Providers**: Each component publishes its internal state as metrics
- **Lock-Free Collection**: Non-blocking metrics collection to prevent performance impact
- **Hierarchical Metrics Organization**: Metrics organized by component type and instance

The metrics system is designed to have minimal impact on the core load generation path while providing rich insights into system behavior.

## 1.4 Request Pipeline Architecture

The framework processes each request through a well-defined pipeline, with each stage handled by specialized components:

### 1.4.1 Pipeline Flow

```
Generation → Transform → Schedule → Sample → Execute → Profile → Collect → Record → Report
```

1. **Generation**: A request specification is created with basic parameters
2. **Transformation**: The request is modified through a series of transformers
3. **Scheduling**: The request is assigned a precise execution time
4. **Sampling**: A decision is made whether to collect detailed data (before execution)
5. **Execution**: The HTTP request is sent to the target system
6. **Profiling**: Kernel and runtime events are captured during execution
7. **Collection**: Results are aggregated into metrics
8. **Recording**: Detailed data is preserved for sampled requests
9. **Reporting**: Results are formatted and presented

Each stage is isolated from others through well-defined interfaces, enabling independent evolution and replacement of components.

### 1.4.2 Cross-Cutting Concerns

Several aspects apply across multiple pipeline stages:

1. **Concurrency Management**: Carefully controlled to prevent client-side bottlenecks
2. **Statistical Validity**: Maintains unbiased measurement throughout
3. **Error Handling**: Tracks and reports errors without interfering with test execution
4. **Resource Management**: Monitors and manages client-side resource utilization
5. **Timing Precision**: Ensures accurate time measurement with minimal jitter
6. **Metrics Collection**: Gathers performance data throughout the pipeline without affecting execution

### 1.4.3 Metrics Flow

The metrics subsystem operates alongside the main request pipeline:

1. **Component Registration**: Components register with the metrics registry at initialization
2. **Direct Operation Path**: High-throughput components use lock-free metrics structures for internal state
3. **Periodic Collection**: The registry periodically collects snapshots from all components
4. **Metrics Publication**: Collected metrics are made available for monitoring and analysis
5. **Subscription Model**: External systems can subscribe to metrics updates

This architecture ensures that metrics collection has minimal impact on the critical path while providing comprehensive visibility into system behavior.

### 1.4.4 Data Flow Example

To illustrate the pipeline, consider how a single request flows through the system:

1. A **generator** creates a basic GET request to `https://api.example.com/users`
2. A **transformer** adds an authorization header and user-agent
3. The **scheduler** determines this request should be sent at timestamp T
4. The **sampler** decides (using random sampling) to collect detailed data
5. At time T, the **executor** sends the HTTP request
6. The **profiler** traces socket operations and connection states
7. When the response arrives, the **collector** adds the timing to metrics
8. For this sampled request, the **recorder** saves full request/response details
9. The **reporter** updates its display with the latest metrics
10. The **metrics registry** collects component state information in the background

This process repeats for each request, with timing and concurrency carefully managed throughout.

## 1.5 Key Innovations

The framework introduces several technical innovations to the field of load testing:

### 1.5.1 True Server Latency Measurement

By using kernel-level instrumentation, the framework separates server-side latency from client-side overhead, providing true measurement of server performance even under extreme load conditions.

### 1.5.2 Saturation-Aware Load Control

The system dynamically adjusts load generation based on client-side saturation metrics, preventing the "observer effect" where the measurement tool itself distorts results.

### 1.5.3 Statistically Valid Sampling

An unbiased sampling approach allows for detailed analysis of a representative subset of requests without introducing statistical bias, enabling deep insights while maintaining performance.

### 1.5.4 Coordinated Omission Prevention

The scheduling architecture explicitly accounts for "missing" high-latency requests that other tools ignore, providing a true picture of service performance under load.

### 1.5.5 Composable Rate Control

The PID controller approach allows the system to target specific metrics (latency, error rate, external signals) rather than just request rate, enabling more sophisticated test scenarios.

### 1.5.6 High-Performance Observability

The hybrid metrics registry pattern provides comprehensive visibility into system behavior with minimal performance impact, allowing for detailed analysis without compromising test accuracy.

## 1.6 Conclusion

The architecture of this load testing framework represents a sophisticated approach to an often-oversimplified problem. By combining rigorous statistical methodology, precise timing control, deep system insights through kernel-level profiling, and comprehensive observability, it provides a level of measurement accuracy rarely seen in load testing tools.

The modular, trait-based design enables extensive customization without sacrificing core principles, making it adaptable to a wide range of testing scenarios from simple functional validation to complex performance engineering.
