//! Performance utilities and optimizations for CSIL processing

use crate::ast::CsilSpec;
use std::time::{Duration, Instant};

/// Performance metrics for CSIL operations
#[derive(Debug, Default, Clone)]
pub struct PerformanceMetrics {
    pub parse_time: Duration,
    pub validation_time: Duration,
    pub total_rules: usize,
    pub total_services: usize,
    pub total_fields_with_metadata: usize,
    pub memory_peak_mb: Option<u64>,
}

impl PerformanceMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rules_per_second(&self) -> f64 {
        if self.parse_time.is_zero() {
            0.0
        } else {
            self.total_rules as f64 / self.parse_time.as_secs_f64()
        }
    }

    pub fn validation_rules_per_second(&self) -> f64 {
        if self.validation_time.is_zero() {
            0.0
        } else {
            self.total_rules as f64 / self.validation_time.as_secs_f64()
        }
    }
}

/// Performance profiler for CSIL operations
pub struct PerformanceProfiler {
    start_time: Instant,
    parse_start: Option<Instant>,
    validation_start: Option<Instant>,
    metrics: PerformanceMetrics,
}

impl Default for PerformanceProfiler {
    fn default() -> Self {
        Self::new()
    }
}

impl PerformanceProfiler {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            parse_start: None,
            validation_start: None,
            metrics: PerformanceMetrics::new(),
        }
    }

    pub fn start_parsing(&mut self) {
        self.parse_start = Some(Instant::now());
    }

    pub fn end_parsing(&mut self, spec: &CsilSpec) {
        if let Some(start) = self.parse_start {
            self.metrics.parse_time = start.elapsed();
            self.metrics.total_rules = spec.rules.len();
            self.metrics.total_services = spec
                .rules
                .iter()
                .filter(|rule| matches!(rule.rule_type, crate::ast::RuleType::ServiceDef(_)))
                .count();
        }
    }

    pub fn start_validation(&mut self) {
        self.validation_start = Some(Instant::now());
    }

    pub fn end_validation(&mut self) {
        if let Some(start) = self.validation_start {
            self.metrics.validation_time = start.elapsed();
        }
    }

    pub fn set_memory_peak(&mut self, peak_mb: u64) {
        self.metrics.memory_peak_mb = Some(peak_mb);
    }

    pub fn metrics(&self) -> &PerformanceMetrics {
        &self.metrics
    }

    pub fn total_elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }
}

/// Memory usage tracker for large file processing
pub struct MemoryTracker {
    initial_usage: Option<u64>,
    peak_usage: u64,
}

impl Default for MemoryTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryTracker {
    pub fn new() -> Self {
        Self {
            initial_usage: Self::get_current_memory_usage(),
            peak_usage: 0,
        }
    }

    pub fn update_peak(&mut self) {
        if let Some(current) = Self::get_current_memory_usage() {
            self.peak_usage = self.peak_usage.max(current);
        }
    }

    pub fn peak_usage_mb(&self) -> u64 {
        self.peak_usage / (1024 * 1024)
    }

    pub fn memory_delta_mb(&self) -> Option<u64> {
        self.initial_usage
            .map(|initial| (self.peak_usage.saturating_sub(initial)) / (1024 * 1024))
    }

    #[cfg(target_os = "linux")]
    fn get_current_memory_usage() -> Option<u64> {
        use std::fs;

        let status = fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let kb: u64 = parts[1].parse().ok()?;
                    return Some(kb * 1024); // Convert KB to bytes
                }
            }
        }
        None
    }

    #[cfg(not(target_os = "linux"))]
    fn get_current_memory_usage() -> Option<u64> {
        // STUB: Memory tracking not implemented for non-Linux platforms
        None
    }
}

/// Incremental parsing context for resuming interrupted operations
pub struct IncrementalParseContext {
    parsed_rules: Vec<crate::ast::Rule>,
    last_position: crate::lexer::Position,
    checksum: u64,
}

impl Default for IncrementalParseContext {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalParseContext {
    pub fn new() -> Self {
        Self {
            parsed_rules: Vec::new(),
            last_position: crate::lexer::Position {
                line: 1,
                column: 1,
                offset: 0,
            },
            checksum: 0,
        }
    }

    pub fn can_resume(&self, file_checksum: u64) -> bool {
        self.checksum == file_checksum && !self.parsed_rules.is_empty()
    }

    pub fn save_progress(
        &mut self,
        rules: Vec<crate::ast::Rule>,
        position: crate::lexer::Position,
        checksum: u64,
    ) {
        self.parsed_rules = rules;
        self.last_position = position;
        self.checksum = checksum;
    }

    pub fn get_progress(&self) -> (&[crate::ast::Rule], crate::lexer::Position) {
        (&self.parsed_rules, self.last_position)
    }
}

/// Calculate checksum for file content to detect changes
pub fn calculate_content_checksum(content: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_metrics_calculations() {
        let metrics = PerformanceMetrics {
            parse_time: Duration::from_millis(500),
            validation_time: Duration::from_millis(200),
            total_rules: 100,
            ..Default::default()
        };

        assert_eq!(metrics.rules_per_second(), 200.0); // 100 rules / 0.5 seconds
        assert_eq!(metrics.validation_rules_per_second(), 500.0); // 100 rules / 0.2 seconds
    }

    #[test]
    fn test_performance_profiler() {
        let mut profiler = PerformanceProfiler::new();

        profiler.start_parsing();
        std::thread::sleep(Duration::from_millis(10));

        let test_spec = crate::ast::CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![],
        };

        profiler.end_parsing(&test_spec);

        assert!(profiler.metrics().parse_time >= Duration::from_millis(10));
        assert_eq!(profiler.metrics().total_rules, 0);
    }

    #[test]
    fn test_memory_tracker() {
        let mut tracker = MemoryTracker::new();
        tracker.update_peak();

        // Should not panic and should provide reasonable values
        assert!(tracker.peak_usage_mb() < 10000); // Less than 10GB is reasonable
    }

    #[test]
    fn test_content_checksum() {
        let content1 = "Type1 = { field: text }";
        let content2 = "Type1 = { field: text }";
        let content3 = "Type2 = { field: text }";

        assert_eq!(
            calculate_content_checksum(content1),
            calculate_content_checksum(content2)
        );
        assert_ne!(
            calculate_content_checksum(content1),
            calculate_content_checksum(content3)
        );
    }

    #[test]
    fn test_incremental_parse_context() {
        let mut context = IncrementalParseContext::new();

        let checksum1 = calculate_content_checksum("content1");
        let checksum2 = calculate_content_checksum("content2");

        // Initially cannot resume
        assert!(!context.can_resume(checksum1));

        // Save some progress with a dummy rule
        let dummy_rule = crate::ast::Rule {
            name: "TestType".to_string(),
            rule_type: crate::ast::RuleType::TypeDef(crate::ast::TypeExpression::Builtin(
                "text".to_string(),
            )),
            position: crate::lexer::Position {
                line: 1,
                column: 1,
                offset: 0,
            },
        };
        context.save_progress(
            vec![dummy_rule],
            crate::lexer::Position {
                line: 10,
                column: 5,
                offset: 50,
            },
            checksum1,
        );

        // Can resume with same checksum
        assert!(context.can_resume(checksum1));

        // Cannot resume with different checksum
        assert!(!context.can_resume(checksum2));
    }
}
