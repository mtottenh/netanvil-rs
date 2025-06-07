use netanvil_core::TestResult;

pub fn print_results(result: &TestResult) {
    println!();
    println!("━━━ Results ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Duration:        {:>10.2?}", result.duration);
    println!("  Total requests:  {:>10}", result.total_requests);
    println!("  Total errors:    {:>10}", result.total_errors);
    println!("  Request rate:    {:>10.1} req/s", result.request_rate);
    println!("  Error rate:      {:>10.2}%", result.error_rate * 100.0);
    println!();
    println!("  Latency:");
    println!("    p50:           {:>10.2?}", result.latency_p50);
    println!("    p90:           {:>10.2?}", result.latency_p90);
    println!("    p99:           {:>10.2?}", result.latency_p99);
    println!("    max:           {:>10.2?}", result.latency_max);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}
