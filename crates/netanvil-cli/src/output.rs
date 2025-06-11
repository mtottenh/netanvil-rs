use netanvil_core::{Report, TestResult};

pub fn print_results(result: &TestResult) {
    print!("{}", Report::new(result));
}
