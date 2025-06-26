# Test Plan for netanvil-types Crate

## Current Testing Status: ❌ 0% Coverage

No tests currently exist. This violates TDD principles where tests should be written first.

## Test Strategy

Following TDD principles, we should implement tests in phases, with each phase driving the implementation of missing features.

---

## Phase 1: Core Type Safety Tests (Week 1)

### 1.1 Time Abstraction Tests
```rust
#[cfg(test)]
mod time_tests {
    // Test monotonic properties
    #[test]
    fn test_monotonic_instant_ordering() { }
    
    // Test arithmetic operations
    #[test]
    fn test_instant_arithmetic() { }
    
    // Test duration calculations
    #[test]
    fn test_duration_precision() { }
    
    // Test overflow handling
    #[test]
    fn test_arithmetic_overflow() { }
}
```

### 1.2 Identity Type Tests
```rust
#[cfg(test)]
mod identity_tests {
    // Test NodeId uniqueness and formatting
    #[test]
    fn test_node_id_creation() { }
    
    // Test ActorId conversion from NodeId
    #[test]
    fn test_actor_id_from_node_id() { }
    
    // Test serialization/deserialization
    #[test]
    fn test_id_serialization() { }
}
```

### 1.3 Error Type Tests
```rust
#[cfg(test)]
mod error_tests {
    // Test error creation and display
    #[test]
    fn test_error_display() { }
    
    // Test error source chain
    #[test]
    fn test_error_source() { }
    
    // Test error conversions
    #[test]
    fn test_error_from_implementations() { }
}
```

---

## Phase 2: Trait Behavior Tests (Week 2)

### 2.1 ProfilingCapability Tests
```rust
#[cfg(test)]
mod profiling_tests {
    // Test default no-op implementation
    #[test]
    fn test_noop_profiling() { }
    
    // Test profiling context
    #[test]
    fn test_profiling_context() { }
    
    // Test with_profiling wrapper
    #[test]
    fn test_with_profiling_execution() { }
}
```

### 2.2 Mock Trait Implementations
```rust
#[cfg(test)]
mod mock_implementations {
    // Mock RateController for testing
    struct MockRateController { }
    
    #[test]
    fn test_rate_controller_trait_requirements() { }
    
    // Mock RequestScheduler
    struct MockScheduler { }
    
    #[test]
    fn test_scheduler_trait_requirements() { }
    
    // Mock RequestExecutor
    struct MockExecutor { }
    
    #[test]
    fn test_executor_trait_requirements() { }
}
```

---

## Phase 3: Request Processing Tests (Week 3)

### 3.1 Request Type Tests
```rust
#[cfg(test)]
mod request_tests {
    // Test RequestContext creation
    #[test]
    fn test_request_context_builder() { }
    
    // Test RequestSpec variants
    #[test]
    fn test_request_spec_creation() { }
    
    // Test ClientIpSpec behavior
    #[test]
    fn test_client_ip_spec_variants() { }
    
    // Test request metadata
    #[test]
    fn test_request_metadata_tags() { }
}
```

### 3.2 Connection Lifecycle Tests
```rust
#[cfg(test)]
mod connection_tests {
    // Test MaxRequestsSpec behavior
    #[test]
    fn test_max_requests_unlimited() { }
    
    #[test]
    fn test_max_requests_fixed() { }
    
    #[test]
    fn test_max_requests_range() { }
    
    // Test connection configuration
    #[test]
    fn test_connection_lifecycle_config() { }
}
```

---

## Phase 4: Distributed System Tests (Week 4)

### 4.1 Node State Tests
```rust
#[cfg(test)]
mod distributed_tests {
    // Test node state transitions
    #[test]
    fn test_node_state_transitions() { }
    
    // Test node role changes
    #[test]
    fn test_node_role_transitions() { }
    
    // Test load assignment
    #[test]
    fn test_load_assignment_validation() { }
}
```

### 4.2 Serialization Tests
```rust
#[cfg(test)]
mod serialization_tests {
    // Test all distributed types serialize correctly
    #[test]
    fn test_node_info_serialization() { }
    
    #[test]
    fn test_peer_info_serialization() { }
    
    // Test binary compatibility
    #[test]
    fn test_binary_format_stability() { }
}
```

---

## Phase 5: Session Management Tests (Week 5)

### 5.1 Session Lifecycle Tests
```rust
#[cfg(test)]
mod session_tests {
    // Test session state machine
    #[test]
    fn test_session_state_transitions() { }
    
    // Test session events
    #[test]
    fn test_session_event_handling() { }
    
    // Test client behavior profiles
    #[test]
    fn test_client_behavior_variants() { }
}
```

### 5.2 Think Time Tests
```rust
#[cfg(test)]
mod think_time_tests {
    // Test fixed think time
    #[test]
    fn test_fixed_think_time() { }
    
    // Test random think time
    #[test]
    fn test_random_think_time_bounds() { }
    
    // Test distribution-based think time
    #[test]
    fn test_distribution_think_time() { }
}
```

---

## Phase 6: Integration Tests (Week 6)

### 6.1 Type Compatibility Tests
```rust
// tests/type_compatibility.rs
#[test]
fn test_all_traits_implement_profiling_capability() { }

#[test]
fn test_all_types_are_send_sync() { }

#[test]
fn test_all_errors_implement_std_error() { }
```

### 6.2 Builder Pattern Tests
```rust
// tests/builders.rs
#[test]
fn test_request_spec_builder() { }

#[test]
fn test_session_config_builder() { }

#[test]
fn test_connection_config_builder() { }
```

---

## Phase 7: Property-Based Tests (Week 7)

### 7.1 Using proptest or quickcheck
```rust
#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;
    
    // Test monotonic instant properties
    proptest! {
        #[test]
        fn test_instant_ordering_transitive(a: u64, b: u64, c: u64) {
            // If a < b and b < c, then a < c
        }
    }
    
    // Test ID generation properties
    proptest! {
        #[test]
        fn test_node_id_roundtrip(s: String) {
            // NodeId can be created and converted back
        }
    }
}
```

---

## Phase 8: Performance Tests (Week 8)

### 8.1 Micro-benchmarks
```rust
#[cfg(test)]
mod bench {
    use test::Bencher;
    
    #[bench]
    fn bench_instant_creation(b: &mut Bencher) { }
    
    #[bench]
    fn bench_request_spec_creation(b: &mut Bencher) { }
    
    #[bench]
    fn bench_node_id_hashing(b: &mut Bencher) { }
}
```

---

## Test Infrastructure Requirements

### 1. Test Utilities Module
Create `src/test_utils.rs`:
```rust
#[cfg(test)]
pub mod test_utils {
    // Factory functions for test data
    pub fn create_test_request_context() -> RequestContext { }
    pub fn create_test_node_info() -> NodeInfo { }
    
    // Assertion helpers
    pub fn assert_profiling_capability<T: ProfilingCapability>(_: &T) { }
}
```

### 2. Documentation Tests
Ensure all examples in documentation compile and run:
```rust
/// Example:
/// ```
/// use netanvil_types::NodeId;
/// let node_id = NodeId::new("test-node");
/// assert_eq!(node_id.as_str(), "test-node");
/// ```
```

### 3. Compile-Time Tests
```rust
// Ensure traits have correct bounds
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_all_traits() {
        assert_send_sync::<Box<dyn RateController>>();
        assert_send_sync::<Box<dyn RequestScheduler>>();
        // etc...
    }
};
```

---

## Testing Priorities

1. **Critical Path First**: Test the core traits and types that other crates depend on
2. **Type Safety**: Ensure strong typing prevents misuse
3. **Thread Safety**: Verify Send + Sync implementations
4. **Serialization**: Ensure distributed types serialize correctly
5. **Performance**: Validate that types have minimal overhead

## Success Metrics

- [ ] 100% coverage of public API
- [ ] All examples in docs are tested
- [ ] Property tests for critical invariants
- [ ] Performance benchmarks establish baselines
- [ ] Integration tests verify type interactions
- [ ] No unsafe code without safety tests

## Continuous Testing

- Run tests on every commit
- Use cargo-tarpaulin for coverage reports
- Use cargo-mutants for mutation testing
- Regular performance regression tests
- Fuzz testing for serialization code