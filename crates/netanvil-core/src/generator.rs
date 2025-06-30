use netanvil_types::{HttpRequestSpec, RequestContext, RequestGenerator};

/// Simple request generator that round-robins through configured URLs.
pub struct SimpleGenerator {
    targets: Vec<String>,
    method: http::Method,
    index: usize,
}

impl SimpleGenerator {
    pub fn new(targets: Vec<String>, method: http::Method) -> Self {
        assert!(!targets.is_empty(), "at least one target URL required");
        Self {
            targets,
            method,
            index: 0,
        }
    }

    pub fn get(targets: Vec<String>) -> Self {
        Self::new(targets, http::Method::GET)
    }
}

impl RequestGenerator for SimpleGenerator {
    type Spec = HttpRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> HttpRequestSpec {
        let url = self.targets[self.index % self.targets.len()].clone();
        self.index += 1;
        HttpRequestSpec {
            method: self.method.clone(),
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        if !targets.is_empty() {
            self.targets = targets;
            self.index = 0;
        }
    }
}
