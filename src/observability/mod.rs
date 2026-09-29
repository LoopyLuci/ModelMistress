use prometheus::{Encoder, TextEncoder, Registry, Counter, Histogram, Gauge};
use tracing::info;

// ============================================================================
// Observability - Prometheus Metrics & OpenTelemetry Integration
// ============================================================================

#[derive(Clone)]
pub struct Observability {
    registry: Registry,
    request_counter: Counter,
    request_duration: Histogram,
    active_requests: Gauge,
    backend_health: Gauge,
}

impl Observability {
    pub fn new() -> Self {
        let registry = Registry::new();
        
        let request_counter = Counter::with_opts(
            prometheus::opts!(
                "model_mistress_requests_total",
                "Total number of requests processed"
            )
        ).unwrap();
        
        let request_duration = Histogram::with_opts(
            prometheus::histogram_opts!(
                "model_mistress_request_duration_seconds",
                "Request duration in seconds"
            ).buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0])
        ).unwrap();
        
        let active_requests = Gauge::with_opts(
            prometheus::opts!(
                "model_mistress_active_requests",
                "Number of currently active requests"
            )
        ).unwrap();
        
        let backend_health = Gauge::with_opts(
            prometheus::opts!(
                "model_mistress_backend_health",
                "Backend health status (1=healthy, 0=unhealthy)"
            )
        ).unwrap();
        
        registry.register(Box::new(request_counter.clone())).unwrap();
        registry.register(Box::new(request_duration.clone())).unwrap();
        registry.register(Box::new(active_requests.clone())).unwrap();
        registry.register(Box::new(backend_health.clone())).unwrap();
        
        Self {
            registry,
            request_counter,
            request_duration,
            active_requests,
            backend_health,
        }
    }
    
    pub fn record_request(&self, duration: f64) {
        self.request_counter.inc();
        self.request_duration.observe(duration);
    }
    
    pub fn start_request(&self) {
        self.active_requests.inc();
    }
    
    pub fn end_request(&self) {
        self.active_requests.dec();
    }
    
    pub fn set_backend_health(&self, healthy: bool) {
        if healthy {
            self.backend_health.set(1.0);
        } else {
            self.backend_health.set(0.0);
        }
    }
    
    pub async fn metrics_handler(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap()
    }
}
