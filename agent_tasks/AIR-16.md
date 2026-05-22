# Task description

The task is to add metrics.


# General principles
1. Every major process must be measured.
2. We take measurements at the edge points so we don't write too many metric calls every time.
3. Add basic low-cardinality labels to the metrics.
4. For time we use Summary.
5. For gauge we use a time-integrating gauge — see the description below (except for CPU and max memory).
6. Instead of using metric vectors — we drop counters straight into the edge points.

# How are we going to do it?
1. Add https://github.com/tikv/rust-prometheus
2. We create a separate monitoring crate where we put a wrapper class for metrics management.
   From it, child local objects will be created that we thread into the flow, storages and sinks, holding the
   counters and other meters of the metrics.
3. All metric calls must hide the implementation inside. That is, inside sinks, sources, flow, etc., strictly typed
   methods with a clear business name are called.

# How do we measure time?
For time we use Summary.
Since there is no Summary in rust-prometheus, we assemble our own based on https://github.com/hdrhistogram/hdrhistogram_rust
or some ddsketch / t-digest implementation.
Run a separate agent to investigate the question and pick a library that is both maintained and has enough stars.

In the Summary object we implement the Collector interface and eviction based on summary-window.

We measure both individual values and the overall total over time (i.e. the overall distribution across all flows, etc.).


Why this way?
imagine we have buckets and we have a bucket from 1 to 2) but the max value is 1.05 — in that case the quantile
will be off by up to 95%.
And if we aggregate, we will estimate by the upper-bound estimate. and we get a more accurate estimate of the
distribution than on histograms.
That is, the goal is to get a smaller error value.

# What are the configs?
For configuration, in the root we get an object
```toml
[metrics.prometheus]
enabled = true # default false
port = 8080 # same default value
prefix = "/mertics" # same default value
summary = {
    window = "5s", # same default value
    quantiles = ["0.5", "0.9", "0.99"]  # same default value
}

```

# At which edge points do we work?
1. in flow (we put into labels the flow name, the sink/source/storage name for the corresponding summary)
   a. measure fetch, transform and sink duration (via summary)
   b. number of rows downloaded, saved (with labels by delete/upsert)
   c. number of errors with the type of error
2. in sinks, sources, storages we count pool sizes and the number of active connections (for this we use the integral gauge from the section about gauges)
3. for the process — we measure processor time, RAM usage, CPU count, start time
it will look approximately like this
```
let mut system = self.system.lock();
        system.refresh_all();

        let pid = sysinfo::get_current_pid().map_err(anyhow::Error::msg)?;
        let process = system
            .process(pid)
            .ok_or(anyhow::Error::msg("Process not found"))?;

        self.memory_gauge
            .with_label_values(&["available_memory"])
            .set(system.available_memory() as f64);
        self.memory_gauge
            .with_label_values(&["used_memory"])
            .set(system.used_memory() as f64);
        self.memory_gauge
            .with_label_values(&["free_memory"])
            .set(system.free_memory() as f64);
        self.memory_gauge
            .with_label_values(&["total_memory"])
            .set(system.total_memory() as f64);

        self.cpu_count.set(system.cpus().len() as f64);

        self.process_cpu_usage.set(process.cpu_usage() as f64);
        self.process_memory_usage.set(process.memory() as f64);
        self.process_start_time.set(process.start_time() as f64);
```

# Time Integrating Gauge
For measuring gauges, instead of an ordinary gauge we use a separate entity.
What's the point of it? We present it in such a way that d(metric)/dt would give the average value over the interval (average over time).
How do we do it?
1. inside we store an accumulator + last value + time of the last measurement.
on each measurement we increase the accumulator by last value + time of the last measurement using Kahan summation.
3. on each scrape we update the accumulator — we assume that a measurement equal to the last one occurred, and add the product by time
and we scrape the accumulator.

on scrape it will look like an ordinary counter.

we add the suffix seconds_integral

# Extra requirements
1.  Before starting execution, translate the task into English.
2. After completing the task, run validator agents.
3. add a section about metrics to the project conventions skill (at the same time, start extracting parts into links... you can also reorganise other sections if they are too large)
4. add to the brief the requirement — that processes must be observable
5. at the end don't forget to run all tests through nextest
6. also try to write unit tests for the moments where tests may be needed
7. reason strictly in English (chat with me in the language in which I'm chatting with you)
8. there is also a requirement that we do not create objects half-built, that is, first we parse the configs and then we immediately create a ready object
9. if prometheus is set to false, then we do not expose the endpoint and we do not take measurements (that is, all metric meters see false and quickly return)
