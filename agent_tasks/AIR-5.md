# Adding Redis

In the first version we add only the sink.


Insert specifics:
1 -- add a `mode` field which defaults to `kv`

but there can also be other values
kv -- 2 required fields key, value, ttl (optional)
kv-delete -- only the key is required (we just do a delete)
list -- only the value is required, the key is optional; if not specified -- we write by prefix
stream -- key and value are required
pubsub -- value required and optional key


into the value of the `to` field we write a prefix

`mode` is added to the sink section

for ttl the format is always interval, for value the format is json,
and for the key the format is string

# Format
For now the data format is json only

and in examples it is preferable to use our scripting language right away
because otherwise it is hard to express json

it will look roughly like this
```yml
flows:
  users:
    sink:
      name: redis
      mode: kv

    compute-mapping:
      key : `_id`
      ttl : 10s
      value : {
         "key" : `value1`,
         "key2" : `value2`
      }
```

that is, we assemble the object in request format right away and it should in principle already work now)

# Structure
We add it as usual into sinks, commons
We add a tag for correct deletion like in the other DBs
Don't forget about CI

on conflict is forbidden and makes no sense) always upsert, and if it is present -- then fail
the sink schema is always known by default based on mode and does not require DB access

# Pool
The sink uses the standard `deadpool-redis` connection pool over
https://github.com/redis-rs/redis-rs — a classic checkout/recycle pool that
hands out one connection per checkout (a whole-batch pipeline rides one
connection in a single round-trip). The earlier idea of a custom multiplexed
"square width × depth" pool was dropped: pipelining a whole batch already
fully occupies one connection, so multiplexing within a connection added no
benefit. The pool config nests under the sink (`config = { url, pool = { max-connections, … } }`).

# Misc
1. add examples to example too
2. update the version only if the linter complains
3. after the task run validator agents, preferably all of them
4. work by the scheme 1 writer at a time. (only 1 agent writes the task, no more)
5. before starting the task translate the text of this file in place to English
6. make tests right away both as e2e with the sink and in app as the full version
7. in all containers use valkey right away
