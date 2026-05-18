-- Runs on first boot of the Postgres container (Linux side, regardless of
-- host OS). Creates the two databases air-elt needs.
CREATE DATABASE airdata;
CREATE DATABASE airstate;
