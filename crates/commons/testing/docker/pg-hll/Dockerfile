# Stock Postgres 16 with the HyperLogLog extension layered on.
#
# We need `hll` for the HLL e2e suite but stock postgres:16 doesn't ship it
# and the citusdata/citus image is too heavy (Citus's per-connection
# routing layer adds noticeable latency to every query — visible in
# nextest where each test is its own process).
FROM postgres:16
RUN apt-get update \
    && apt-get install -y --no-install-recommends postgresql-16-hll \
    && rm -rf /var/lib/apt/lists/*
