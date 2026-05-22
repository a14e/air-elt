use prometheus::IntGauge;

/// Plain gauges for the four basic registry-shape cardinalities. Set
/// once after the registry is assembled. No `_count` or `_total`
/// suffix: these are bare `IntGauge`s holding the cardinality of the
/// configured registry, not Prometheus `Counter`s and not the `_count`
/// half of a Summary/Histogram `_sum`/`_count` pair. `_count` is
/// reserved by the exposition format for that Summary/Histogram pair
/// and reusing it on a plain gauge causes tooling collisions; `_total`
/// is reserved for monotonic counters consumers feed into `rate()`. A
/// suffix-free name is the honest fit here.
#[derive(Clone)]
pub struct CountsCollector {
    pub flows: IntGauge,
    pub sources: IntGauge,
    pub sinks: IntGauge,
    pub storages: IntGauge,
}

impl CountsCollector {
    pub fn new(registry: &prometheus::Registry) -> prometheus::Result<Self> {
        let flows = IntGauge::new("flows", "Number of configured flows")?;
        let sources = IntGauge::new("sources", "Number of configured sources")?;
        let sinks = IntGauge::new("sinks", "Number of configured sinks")?;
        let storages = IntGauge::new("storages", "Number of configured storages")?;

        registry.register(Box::new(flows.clone()))?;
        registry.register(Box::new(sources.clone()))?;
        registry.register(Box::new(sinks.clone()))?;
        registry.register(Box::new(storages.clone()))?;

        Ok(Self {
            flows,
            sources,
            sinks,
            storages,
        })
    }

    pub fn set(&self, flows: u32, sources: u32, sinks: u32, storages: u32) {
        self.flows.set(i64::from(flows));
        self.sources.set(i64::from(sources));
        self.sinks.set(i64::from(sinks));
        self.storages.set(i64::from(storages));
    }
}
