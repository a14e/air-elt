-- Target schema for the manual mongo → postgres test.
-- Applied against the `airdata` database (created by compose-init/init.sh).

-- The mongodb source infers schema by sampling and treats every field as
-- nullable, so most sink columns are nullable too. `email` is deliberately
-- left NOT NULL to exercise the mapping `default` clause — see
-- flows/users.toml.
--
-- The 10-column shape is intentional: it covers a representative spread of
-- the type matrix (text / bigint / boolean / integer / float / numeric /
-- jsonb / timestamptz) so the smoke run also serves as a quick cross-type
-- sanity check on top of the throughput / resource-cost numbers.
CREATE TABLE IF NOT EXISTS public.users (
    id          text            PRIMARY KEY,
    seq         bigint,
    name        text,
    email       text            NOT NULL,
    is_active   boolean,
    age         integer,
    score       double precision,
    balance     numeric(12, 2),
    tags        jsonb,
    inserted_at timestamptz
);

CREATE INDEX IF NOT EXISTS users_seq_idx ON public.users (seq);
